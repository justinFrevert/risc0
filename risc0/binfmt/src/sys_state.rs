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

extern crate alloc;

use alloc::{collections::VecDeque, vec::Vec};
use core::fmt;

use risc0_zkp::core::{digest::Digest, hash::sha::Sha256};
use serde::{Deserialize, Serialize};

#[cfg(not(target_os = "zkvm"))]
use crate::MemoryImage;
use crate::{tagged_struct, Digestible};

/// Represents the public state of a segment, needed for continuations and
/// receipt verification.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SystemState {
    /// The program counter.
    pub pc: u32,

    /// The root hash of a merkle tree which confirms the
    /// integrity of the memory image.
    pub merkle_root: Digest,
}

impl SystemState {
    pub fn decode(flat: &mut VecDeque<u32>) -> Result<Self, DecodeError> {
        Ok(Self {
            pc: read_u32_bytes(flat)?,
            merkle_root: read_sha_halfs(flat)?,
        })
    }

    pub fn encode(&self, flat: &mut Vec<u32>) {
        write_u32_bytes(flat, self.pc);
        write_sha_halfs(flat, &self.merkle_root);
    }
}

impl Eq for SystemState {}

impl Digestible for SystemState {
    /// Hash the [crate::SystemState] to get a digest of the struct.
    fn digest<S: Sha256>(&self) -> Digest {
        tagged_struct::<S>("risc0.SystemState", &[self.merkle_root], &[self.pc])
    }
}

#[cfg(not(target_os = "zkvm"))]
impl From<&MemoryImage> for SystemState {
    fn from(image: &MemoryImage) -> Self {
        Self {
            pc: image.pc,
            merkle_root: image
                .compute_root_hash()
                .expect("failed to compute root hash"),
        }
    }
}

pub fn read_sha_halfs(flat: &mut VecDeque<u32>) -> Result<Digest, DecodeError> {
    let mut bytes = Vec::<u8>::new();
    if flat.len() < 16 {
        return Err(DecodeError::EndOfStream);
    }
    for half in flat.drain(0..16) {
        bytes.push((half & 0xff).try_into().unwrap());
        bytes.push(
            (half >> 8)
                .try_into()
                .map_err(|_| DecodeError::OutOfRange)?,
        );
    }
    Ok(bytes.try_into().unwrap())
}

fn read_u32_bytes(flat: &mut VecDeque<u32>) -> Result<u32, DecodeError> {
    if flat.len() < 4 {
        return Err(DecodeError::EndOfStream);
    }
    Ok(u32::from_le_bytes(
        flat.drain(0..4)
            .map(|x| x as u8)
            .collect::<Vec<u8>>()
            .try_into()
            .unwrap(),
    ))
}

pub fn write_sha_halfs(flat: &mut Vec<u32>, digest: &Digest) {
    for x in digest.as_words() {
        flat.push(*x & 0xffff);
        flat.push(*x >> 16);
    }
}

fn write_u32_bytes(flat: &mut Vec<u32>, word: u32) {
    for x in word.to_le_bytes() {
        flat.push(x as u32);
    }
}

/// Error returned when a decoding failure occurs.
#[derive(Debug, Copy, Clone)]
pub enum DecodeError {
    /// End of stream was reached when more data was expected.
    EndOfStream,
    /// Value in the stream is outside the expected range.
    OutOfRange,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::EndOfStream => write!(f, "end of stream reached when more data was expected"),
            Self::OutOfRange => write!(f, "value outside of expected range"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for DecodeError {}
