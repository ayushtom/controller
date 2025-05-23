use async_trait::async_trait;
use cainome_cairo_serde::U256;
use ecdsa::Signature;
use k256::Secp256k1;
use starknet::core::types::{EthAddress, Felt};

#[cfg(not(target_arch = "wasm32"))]
use alloy_primitives::eip191_hash_message;
#[cfg(not(target_arch = "wasm32"))]
use alloy_signer::k256::ecdsa::SigningKey;
#[cfg(not(target_arch = "wasm32"))]
use alloy_signer::utils::secret_key_to_address;
#[cfg(not(target_arch = "wasm32"))]
use rand::rngs::OsRng;

use crate::abigen::controller::SignerSignature;
#[cfg(not(target_arch = "wasm32"))]
use crate::abigen::controller::{
    Eip191Signer as ControllerEip191Signer, Signature as ControllerSignature,
};
use crate::signers::{HashSigner, SignError};

#[cfg(target_arch = "wasm32")]
use crate::abigen::controller::{
    Eip191Signer as ControllerEip191Signer, Signature as ControllerSignature,
};
#[cfg(target_arch = "wasm32")]
use crate::signers::external::external_sign_message;
#[cfg(target_arch = "wasm32")]
use hex;

/// A signer that implements EIP-191 signing using the Alloy library
#[derive(Debug, Clone, PartialEq)]
pub struct Eip191Signer {
    #[cfg(not(target_arch = "wasm32"))]
    pub signing_key: SigningKey,
    pub address: EthAddress,
}

impl Eip191Signer {
    #[cfg(not(target_arch = "wasm32"))]
    /// Create a random Eip191Signer
    pub fn random() -> Self {
        let signing_key = SigningKey::random(&mut OsRng);
        let address = secret_key_to_address(&signing_key).0 .0.into();

        Self {
            signing_key,
            address,
        }
    }

    /// Get the Ethereum address of this signer
    pub fn address(&self) -> EthAddress {
        self.address.clone()
    }

    fn construct_signature(
        &self,
        signature: Signature<Secp256k1>,
        y_parity: bool,
    ) -> SignerSignature {
        let (signature, y_parity) = if let Some(normalized) = signature.normalize_s() {
            (normalized, !y_parity)
        } else {
            (signature, y_parity)
        };
        let r_bytes = signature.r().to_bytes();
        let s_bytes = signature.s().to_bytes();

        let mut r_padded = [0u8; 32];
        r_padded[32 - r_bytes.len()..].copy_from_slice(&r_bytes);
        let mut s_padded = [0u8; 32];
        s_padded[32 - s_bytes.len()..].copy_from_slice(&s_bytes);

        let r = U256::from_bytes_be(&r_padded);
        let s = U256::from_bytes_be(&s_padded);

        let eth_address = cainome::cairo_serde::EthAddress(self.address().into());
        let controller_signer = ControllerEip191Signer { eth_address };

        let signature = ControllerSignature { r, s, y_parity };

        SignerSignature::Eip191((controller_signer, signature))
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait]
impl HashSigner for Eip191Signer {
    async fn sign(&self, tx_hash: &Felt) -> Result<SignerSignature, SignError> {
        // Convert Felt to bytes
        let tx_hash_bytes = tx_hash.to_bytes_be();
        let message_hash = eip191_hash_message(tx_hash_bytes);

        // Sign the message hash using the k256 library
        // This returns a recoverable signature
        let recoverable_signature = self
            .signing_key
            .sign_prehash_recoverable(message_hash.as_slice())
            .map_err(|e| SignError::InvalidMessageError(e.to_string()))?;

        // Extract the signature components
        let (signature, recovery_id) = recoverable_signature;

        Ok(self.construct_signature(signature, recovery_id.is_y_odd()))
    }
}

#[cfg(target_arch = "wasm32")]
#[async_trait(?Send)]
impl HashSigner for Eip191Signer {
    async fn sign(&self, tx_hash: &Felt) -> Result<SignerSignature, SignError> {
        let address_hex = format!("0x{}", hex::encode(self.address.clone().as_bytes()));

        let tx_hash_prefixed = format!("0x{:0>63}", hex::encode(tx_hash.to_bytes_be()));

        let signature_hex = external_sign_message(&address_hex, &tx_hash_prefixed).await?;
        let signature_bytes = hex::decode(signature_hex.trim_start_matches("0x")).map_err(|e| {
            SignError::BridgeError(format!("Failed to decode signature hex: {}", e))
        })?;

        if signature_bytes.len() != 65 {
            return Err(SignError::BridgeError(format!(
                "Invalid signature length from bridge: expected 65, got {}",
                signature_bytes.len()
            )));
        }

        let v = signature_bytes[64];
        let y_parity = match v {
            0 | 27 => false,                   // y even
            1 | 28 => true,                    // y odd
            v if v >= 35 => (v - 35) % 2 == 1, // EIP-155
            _ => return Err(SignError::BridgeError(format!("unsupported v value {}", v))),
        };

        let signature =
            Signature::<Secp256k1>::from_bytes(signature_bytes[0..64].into()).map_err(|e| {
                SignError::BridgeError(format!("Failed to decode signature bytes: {}", e))
            })?;

        Ok(self.construct_signature(signature, y_parity))
    }
}

impl From<Eip191Signer> for crate::signers::Signer {
    fn from(signer: Eip191Signer) -> Self {
        crate::signers::Signer::Eip191(signer)
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod wasm_tests {
    use super::*;
    use starknet::core::types::Felt;
    use std::str::FromStr;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_test::*;

    // Configure wasm-bindgen test runner
    wasm_bindgen_test_configure!(run_in_browser);

    // Define the mock wallet bridge JS interface
    #[wasm_bindgen(inline_js = r#"
        export function setup_mock_keychain_wallets() {
            if (!window.keychain_wallets) { window.keychain_wallets = {}; }

            window.keychain_wallets.signMessage = (identifier, message) => {
                console.log(`Mock bridge called: signMessage(${identifier}, ${message})`);

                // --- Define Expected Test Values ---
                const expectedIdentifier = "0xa517d17bc2cca98e94f9975efd295ec70bfb3cf7";
                const mockSignature = "0xbbd22f35065dbb1af67c2ff18da7b16f70df83f428a4fbfb84552b857307654e320be9840b7cd53a0f6997784a76afa4f2eb07d2caa6f4330c90a81585ee059b1c";

                return new Promise((resolve, reject) => {
                    // Check if the identifier matches the expected one for the success test
                    if (identifier === expectedIdentifier) {
                        const response = {
                            success: true,
                            wallet: "metamask",
                            result: mockSignature,
                            error: null,
                            account: identifier
                        };
                        console.log("Mock bridge resolving successfully for:", identifier);
                        resolve(response); // Resolve with success data
                    } else {
                        // Handle the error case for test_eip191_wasm_sign_bridge_error
                        const errorMessage = `Bridge Error: Unexpected input: ${identifier}`;
                        console.error("Mock bridge rejecting for:", identifier, errorMessage);
                        // Reject the promise for error cases, as expected by wasm-bindgen(catch)
                        reject(new Error(errorMessage));
                    }
                });
            };

            console.log("Mock keychain_wallets setup complete.", JSON.stringify(window.keychain_wallets, null, 2));
        }
    "#)]

    extern "C" {
        fn setup_mock_keychain_wallets();
    }

    #[wasm_bindgen_test]
    async fn test_eip191_wasm_sign_success() {
        // Setup the mock bridge in the JS environment
        setup_mock_keychain_wallets();

        let address_hex = "0xa517d17bc2cca98e94f9975efd295ec70bfb3cf7";
        let address = EthAddress::from_str(address_hex).expect("Failed to parse test address");
        let tx_hash = Felt::from_hex_unchecked(
            "0x5468500fa6b5e101a5d59da8003f3b4fc0789f1f629437d411cff1af236ac9",
        );

        // Create the signer instance
        let signer = Eip191Signer { address };

        // --- Execute Test ---
        let result = signer.sign(&tx_hash).await;

        // --- Assertions ---
        assert!(result.is_ok(), "Signing failed: {:?}", result.err());
        match result.unwrap() {
            SignerSignature::Eip191((controller_signer, signature)) => {
                assert_eq!(
                    controller_signer.eth_address.0,
                    Felt::from_str(address_hex).expect("Failed to parse test address"),
                    "Signer address mismatch"
                );

                // Create expected r from bytes
                let expected_r_bytes =
                    hex::decode("bbd22f35065dbb1af67c2ff18da7b16f70df83f428a4fbfb84552b857307654e")
                        .unwrap();
                let expected_r = U256::from_bytes_be(&expected_r_bytes.try_into().unwrap());
                assert_eq!(signature.r, expected_r, "Signature R mismatch");

                // Create expected s from bytes
                let expected_s_bytes =
                    hex::decode("320be9840b7cd53a0f6997784a76afa4f2eb07d2caa6f4330c90a81585ee059b")
                        .unwrap();
                let expected_s = U256::from_bytes_be(&expected_s_bytes.try_into().unwrap());
                assert_eq!(signature.s, expected_s, "Signature S mismatch");

                assert!(signature.y_parity, "Signature Y-Parity mismatch");
            }
            _ => panic!("Unexpected SignerSignature variant"),
        }
    }

    #[wasm_bindgen_test]
    async fn test_eip191_wasm_sign_keychain_wallets_error() {
        // Setup the mock bridge
        setup_mock_keychain_wallets();

        // Use a different address to trigger the error case in the mock bridge
        let wrong_address_hex = "0xffffffffffffffffffffffffffffffffffffffff";
        let address =
            EthAddress::from_str(wrong_address_hex).expect("Failed to parse wrong address");
        let tx_hash = Felt::from_hex_unchecked("0x12345");

        let signer = Eip191Signer { address };

        // Execute
        let result = signer.sign(&tx_hash).await;

        // Assertions
        assert!(result.is_err(), "Signing should have failed");
        match result.err().unwrap() {
            SignError::BridgeError(msg) => {
                assert!(
                    msg.contains("Unexpected input"),
                    "Error message mismatch: {}",
                    msg
                );
                assert!(
                    msg.contains(wrong_address_hex),
                    "Error message should contain wrong address: {}",
                    msg
                );
            }
            e => panic!("Unexpected error type: {:?}", e),
        }
    }
}
