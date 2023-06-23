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

use password_checker_core::PasswordRequest;
use password_checker_methods::{PW_CHECKER_ELF, PW_CHECKER_ID};
use rand::prelude::*;
use risc0_zkvm::{
    serde::{from_slice, to_vec},
    sha::Digest,
    Executor, ExecutorEnv,
};

fn main() {
    let mut rng = StdRng::from_entropy();
    let mut salt = [0u8; 32];
    rng.fill_bytes(&mut salt);

    let request = PasswordRequest {
        password: "S00perSecr1t!!!".into(),
        salt,
    };

    let password_hash = password_checker(request);
    println!("Password hash is: {}", &password_hash);
}

fn password_checker(request: PasswordRequest) -> Digest {
    let env = ExecutorEnv::builder()
        .add_input(&to_vec(&request).unwrap())
        .build()
        .unwrap();

    let mut exec = Executor::from_elf(env, PW_CHECKER_ELF).unwrap();
    let session = exec.run().unwrap();

    let receipt = session.prove().unwrap();

    receipt.verify(PW_CHECKER_ID.into()).unwrap();

    from_slice(&receipt.get_journal()).unwrap()
}

#[cfg(test)]
mod tests {
    use password_checker_core::PasswordRequest;

    #[test]
    fn main() {
        const TEST_SALT: [u8; 32] = [0u8; 32];
        const TEST_PASSWORD: &str = "S00perSecr1t!!!";

        let request = PasswordRequest {
            password: TEST_PASSWORD.into(),
            salt: TEST_SALT,
        };

        super::password_checker(request);
    }
}
