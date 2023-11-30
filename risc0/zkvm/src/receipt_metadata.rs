// Copyright 2023 RISC Zero, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! [ReceiptMetadata] and associated types and functions.
//!
//! A [ReceiptMetadata] struct contains the public claims about a zkVM guest
//! execution, such as the journal committed to by the guest. It also includes
//! important information such as the exit code and the starting and ending
//! system state (i.e. the state of memory).

use alloc::{collections::VecDeque, vec::Vec};
use core::{fmt, ops::Deref};

use anyhow::{anyhow, ensure};
use risc0_binfmt::{read_sha_halfs, tagged_list, tagged_list_cons, tagged_struct, write_sha_halfs};
use serde::{Deserialize, Serialize};

use crate::{
    sha::{self, Digest, Digestible, Sha256},
    SystemState,
};

use codec::{Encode, Decode};

/// Public claims about a zkVM guest execution, such as the journal committed to by the guest.
///
/// Also includes important information such as the exit code and the starting and ending system
/// state (i.e. the state of memory). [ReceiptMetadata] is a "Merkle-ized struct" supporting
/// partial openings of the underlying fields from a hash commitment to the full structure. Also
/// see [MaybePruned].
#[derive(Clone, Debug, Serialize, Deserialize, Encode, Decode)]
#[cfg_attr(test, derive(PartialEq))]
pub struct ReceiptMetadata {
    /// The [SystemState] of a segment just before execution has begun.
    pub pre: MaybePruned<SystemState>,

    /// The [SystemState] of a segment just after execution has completed.
    pub post: MaybePruned<SystemState>,

    /// The exit code for a segment
    pub exit_code: ExitCode,

    /// Input to the guest.
    ///
    /// NOTE: This field can only be constructed as a Digest because it is not yet
    /// cryptographically bound by the RISC Zero proof system; the guest has no way to set the
    /// input. In the future, it will be implemented with a [MaybePruned] type.
    // TODO(1.0): Determine the 1.0 status of input.
    pub input: Digest,

    /// A [Output] of the guest, including the journal and assumptions set
    /// during execution.
    pub output: MaybePruned<Option<Output>>,
}

impl ReceiptMetadata {
    /// Decode a [crate::ReceiptMetadata] from a list of [u32]'s
    pub fn decode(flat: &mut VecDeque<u32>) -> Result<Self, InvalidExitCodeError> {
        let input = read_sha_halfs(flat);
        let pre = SystemState::decode(flat);
        let post = SystemState::decode(flat);
        let sys_exit = flat.pop_front().unwrap();
        let user_exit = flat.pop_front().unwrap();
        let exit_code = ExitCode::from_pair(sys_exit, user_exit)?;
        let output = read_sha_halfs(flat);

        Ok(Self {
            input,
            pre: pre.into(),
            post: post.into(),
            exit_code,
            output: MaybePruned::Pruned(output),
        })
    }

    /// Encode a [crate::ReceiptMetadata] to a list of [u32]'s
    pub fn encode(&self, flat: &mut Vec<u32>) -> Result<(), PrunedValueError> {
        write_sha_halfs(flat, &self.input);
        self.pre.as_value()?.encode(flat);
        self.post.as_value()?.encode(flat);
        let (sys_exit, user_exit) = self.exit_code.into_pair();
        flat.push(sys_exit);
        flat.push(user_exit);
        write_sha_halfs(flat, &self.output.digest());
        Ok(())
    }
}

impl risc0_binfmt::Digestible for ReceiptMetadata {
    /// Hash the [crate::ReceiptMetadata] to get a digest of the struct.
    fn digest<S: Sha256>(&self) -> Digest {
        let (sys_exit, user_exit) = self.exit_code.into_pair();
        tagged_struct::<S>(
            "risc0.ReceiptMeta",
            &[
                self.input,
                self.pre.digest(),
                self.post.digest(),
                self.output.digest(),
            ],
            &[sys_exit, user_exit],
        )
    }
}

/// Indicates how a Segment or Session's execution has terminated
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Encode, Decode)]
pub enum ExitCode {
    /// This indicates when a system-initiated split has occurred due to the
    /// segment limit being exceeded.
    SystemSplit,

    /// This indicates that the session limit has been reached.
    ///
    /// NOTE: This state is reported by the host prover and results in the same proof as an
    /// execution ending in `SystemSplit`.
    // TODO(1.0): Refine how we handle the difference between proven and unproven exit codes.
    SessionLimit,

    /// A user may manually pause a session so that it can be resumed at a later
    /// time, along with the user returned code.
    Paused(u32),

    /// This indicates normal termination of a program with an interior exit
    /// code returned from the guest.
    Halted(u32),

    /// This indicates termination of a program where the next instruction will
    /// fail due to a machine fault (e.g. out of bounds memory read).
    ///
    /// NOTE: This state is reported by the host prover and results in the same proof as an
    /// execution ending in `SystemSplit`.
    // TODO(1.0): Refine how we handle the difference between proven and unproven exit codes.
    Fault,
}

impl ExitCode {
    pub(crate) fn into_pair(self) -> (u32, u32) {
        match self {
            ExitCode::Halted(user_exit) => (0, user_exit),
            ExitCode::Paused(user_exit) => (1, user_exit),
            ExitCode::SystemSplit => (2, 0),
            // NOTE: SessionLimit and Fault result in the same exit code set by the rv32im
            // circuit. As a result, this conversion is lossy. This factoring results in Fault,
            // SessionLimit, and SystemSplit all having the same digest.
            ExitCode::SessionLimit => (2, 0),
            ExitCode::Fault => (2, 0),
        }
    }

    pub(crate) fn from_pair(
        sys_exit: u32,
        user_exit: u32,
    ) -> Result<ExitCode, InvalidExitCodeError> {
        match sys_exit {
            0 => Ok(ExitCode::Halted(user_exit)),
            1 => Ok(ExitCode::Paused(user_exit)),
            2 => Ok(ExitCode::SystemSplit),
            _ => Err(InvalidExitCodeError(sys_exit, user_exit)),
        }
    }

    #[cfg(not(target_os = "zkvm"))]
    pub(crate) fn expects_output(&self) -> bool {
        match self {
            ExitCode::Halted(_) | ExitCode::Paused(_) => true,
            ExitCode::SystemSplit | ExitCode::SessionLimit | ExitCode::Fault => false,
        }
    }
}

/// Error returned when a (system, user) exit code pair is an invalid
/// representation.
#[derive(Debug, Copy, Clone)]
pub struct InvalidExitCodeError(pub u32, pub u32);

impl fmt::Display for InvalidExitCodeError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "invalid exit code pair ({}, {})", self.0, self.1)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for InvalidExitCodeError {}

/// Output field in the [ReceiptMetadata], committing to a claimed journal and assumptions list.
#[derive(Clone, Debug, Serialize, Deserialize, Encode, Decode)]
#[cfg_attr(test, derive(PartialEq))]
pub struct Output {
    /// The journal committed to by the guest execution.
    pub journal: MaybePruned<Vec<u8>>,

    /// An ordered list of [ReceiptMetadata] digests corresponding to the
    /// calls to `env::verify` and `env::verify_integrity`.
    ///
    /// Verifying the integrity of a [crate::Receipt] corresponding to a [ReceiptMetadata] with a
    /// non-empty assumptions list does not guarantee unconditionally any of the claims over the
    /// guest execution (i.e. if the assumptions list is non-empty, then the journal digest cannot
    /// be trusted to correspond to a genuine execution). The claims can be checked by additional
    /// verifying a [crate::Receipt] for every digest in the assumptions list.
    pub assumptions: MaybePruned<Assumptions>,
}

impl risc0_binfmt::Digestible for Output {
    /// Hash the [Output] to get a digest of the struct.
    fn digest<S: Sha256>(&self) -> Digest {
        tagged_struct::<S>(
            "risc0.Output",
            &[self.journal.digest(), self.assumptions.digest()],
            &[],
        )
    }
}

/// A list of assumptions, each a [Digest] of a [ReceiptMetadata].
#[derive(Clone, Default, Debug, Serialize, Deserialize, Encode, Decode)]
#[cfg_attr(test, derive(PartialEq))]
pub struct Assumptions(pub Vec<MaybePruned<ReceiptMetadata>>);

impl Assumptions {
    /// Add an assumption to the head of the assumptions list.
    pub fn add(&mut self, assumption: MaybePruned<ReceiptMetadata>) {
        self.0.insert(0, assumption);
    }

    /// Mark an assumption as resolved and remove it from the list.
    ///
    /// Assumptions can only be removed from the head of the list.
    pub fn resolve(&mut self, resolved: &Digest) -> anyhow::Result<()> {
        let head = self
            .0
            .first()
            .ok_or_else(|| anyhow!("cannot resolve assumption from empty list"))?;

        ensure!(
            &head.digest() == resolved,
            "resolved assumption is not equal to the head of the list: {} != {}",
            resolved,
            head.digest()
        );

        // Drop the head of the assumptions list.
        self.0 = self.0.split_off(1);
        Ok(())
    }
}

impl Deref for Assumptions {
    type Target = [MaybePruned<ReceiptMetadata>];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl risc0_binfmt::Digestible for Assumptions {
    /// Hash the [Output] to get a digest of the struct.
    fn digest<S: Sha256>(&self) -> Digest {
        tagged_list::<S>(
            "risc0.Assumptions",
            &self.0.iter().map(|a| a.digest()).collect::<Vec<_>>(),
        )
    }
}

impl MaybePruned<Assumptions> {
    /// Check if the (possibly pruned) assumptions list is empty.
    pub fn is_empty(&self) -> bool {
        match self {
            MaybePruned::Value(list) => list.is_empty(),
            MaybePruned::Pruned(digest) => digest == &Digest::ZERO,
        }
    }

    /// Add an assumption to the head of the assumptions list.
    ///
    /// If this value is pruned, then the result will also be a pruned value.
    pub fn add(&mut self, assumption: MaybePruned<ReceiptMetadata>) {
        match self {
            MaybePruned::Value(list) => list.add(assumption),
            MaybePruned::Pruned(list_digest) => {
                *list_digest = tagged_list_cons::<sha::Impl>(
                    "risc0.Assumptions",
                    &assumption.digest(),
                    &*list_digest,
                );
            }
        }
    }

    /// Mark an assumption as resolved and remove it from the list.
    ///
    /// Assumptions can only be removed from the head of the list. If this value
    /// is pruned, then the result will also be a pruned value. The `rest`
    /// parameter should be equal to the digest of the list after the
    /// resolved assumption is removed.
    pub fn resolve(&mut self, resolved: &Digest, rest: &Digest) -> anyhow::Result<()> {
        match self {
            MaybePruned::Value(list) => list.resolve(resolved),
            MaybePruned::Pruned(list_digest) => {
                let reconstructed =
                    tagged_list_cons::<sha::Impl>("risc0.Assumptions", resolved, rest);
                ensure!(
                    &reconstructed == list_digest,
                    "reconstructed list digest does not match; expected {}, reconstructed {}",
                    list_digest,
                    reconstructed
                );

                // Set the pruned digest value to be equal to the rest parameter.
                *list_digest = rest.clone();
                Ok(())
            }
        }
    }
}

/// Either a source value or a hash [Digest] of the source value.
///
/// This type supports creating "Merkle-ized structs". Each field of a Merkle-ized struct can have
/// either the full value, or it can be "pruned" and replaced with a digest committing to that
/// value. One way to think of this is as a special Merkle tree of a predefined shape. Each field
/// is a child node. Any field/node in the tree can be opened by providing the Merkle inclusion
/// proof. When a subtree is pruned, the digest commits to the value of all contained fields.
/// [ReceiptMetadata] is the motivating example of this type of Merkle-ized struct.
#[derive(Clone, Deserialize, Serialize, Encode, Decode)]
pub enum MaybePruned<T>
where
    T: Clone + Serialize,
{
    /// Unpruned value.
    Value(T),
    /// Pruned value, which is a hash [Digest] of the value.
    Pruned(Digest),
}

impl<T> MaybePruned<T>
where
    T: Clone + Serialize,
{
    /// Unwrap the value, or return an error.
    pub fn value(self) -> Result<T, PrunedValueError> {
        match self {
            MaybePruned::Value(value) => Ok(value),
            MaybePruned::Pruned(digest) => Err(PrunedValueError(digest)),
        }
    }

    /// Unwrap the value as a reference, or return an error.k
    pub fn as_value(&self) -> Result<&T, PrunedValueError> {
        match self {
            MaybePruned::Value(ref value) => Ok(value),
            MaybePruned::Pruned(ref digest) => Err(PrunedValueError(digest.clone())),
        }
    }
}

impl<T> From<T> for MaybePruned<T>
where
    T: Clone + Serialize,
{
    fn from(value: T) -> Self {
        Self::Value(value)
    }
}

impl<T> Digestible for MaybePruned<T>
where
    T: Digestible + Clone + Serialize,
{
    fn digest(&self) -> Digest {
        match self {
            MaybePruned::Value(ref val) => val.digest(),
            MaybePruned::Pruned(digest) => digest.clone(),
        }
    }
}

impl<T> Default for MaybePruned<T>
where
    T: Digestible + Default + Clone + Serialize,
{
    fn default() -> Self {
        MaybePruned::Value(Default::default())
    }
}

impl<T> MaybePruned<Option<T>>
where
    T: Clone + Serialize,
{
    /// Returns true is the value is None, or the value is pruned as the zero
    /// digest.
    pub fn is_none(&self) -> bool {
        match self {
            MaybePruned::Value(Some(_)) => false,
            MaybePruned::Value(None) => true,
            MaybePruned::Pruned(digest) => digest == &Digest::ZERO,
        }
    }

    /// Returns true is the value is Some(_), or the value is pruned as a
    /// non-zero digest.
    pub fn is_some(&self) -> bool {
        !self.is_none()
    }
}

#[cfg(test)]
impl<T> PartialEq for MaybePruned<T>
where
    T: Clone + Serialize + PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Value(a), Self::Value(b)) => a == b,
            (Self::Pruned(a), Self::Pruned(b)) => a == b,
            _ => false,
        }
    }
}

impl<T> fmt::Debug for MaybePruned<T>
where
    T: Clone + Serialize + risc0_binfmt::Digestible + fmt::Debug,
{
    /// Format [MaybePruned] values are if they were a struct with value and
    /// digest fields. Digest field is always provided so that divergent
    /// trees of [MaybePruned] values can be compared.
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut builder = fmt.debug_struct("MaybePruned");
        if let MaybePruned::Value(value) = self {
            builder.field("value", value);
        }
        builder.field("digest", &self.digest()).finish()
    }
}

/// Error returned when the source value was pruned, and is not available.
#[derive(Debug, Clone)]
pub struct PrunedValueError(pub Digest);

impl fmt::Display for PrunedValueError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "value is pruned: {}", &self.0)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for PrunedValueError {}
