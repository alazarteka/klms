use cbc::cipher::{BlockEncryptMut, KeyIvInit, block_padding::AnsiX923};
use kisaseed::SEED;
use zeroize::Zeroizing;

use crate::error::AppError;

type SeedCbc = cbc::Encryptor<SEED>;

pub fn encrypt_user_data(login_key: &str, json: &[u8]) -> Result<String, AppError> {
    if login_key.len() < 96 {
        return Err(AppError::auth_protocol(
            "KAIST SSO returned a malformed login key",
        ));
    }
    let key = Zeroizing::new(decode_hex(&login_key[..64])?);
    let iv = Zeroizing::new(decode_hex(&login_key[64..96])?);
    // CryptoJS accepts a 256-bit parsed key, but KISA SEED is a 128-bit cipher.
    // Its implementation consumes the first 128 bits, matching the live site.
    let cipher = SeedCbc::new_from_slices(&key[..16], &iv)
        .map_err(|_| AppError::auth_protocol("KAIST SSO returned an invalid login key"))?;
    let encrypted = cipher.encrypt_padded_vec_mut::<AnsiX923>(json);
    Ok(encode_hex(&encrypted))
}

fn decode_hex(value: &str) -> Result<Vec<u8>, AppError> {
    if value.len() % 2 != 0 {
        return Err(AppError::auth_protocol(
            "KAIST SSO returned malformed hexadecimal data",
        ));
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_nibble(pair[0])?;
            let low = hex_nibble(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn hex_nibble(value: u8) -> Result<u8, AppError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(AppError::auth_protocol(
            "KAIST SSO returned malformed hexadecimal data",
        )),
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0xf) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_seed_cbc_vector() {
        let key = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f101112131415161718191a1b1c1d1e1f";
        // Cross-checked against OpenSSL's legacy-provider SEED-CBC output.
        assert_eq!(
            encrypt_user_data(key, b"{}").unwrap(),
            "d558576b3e0adc65644f932e64d5a1e1"
        );
    }
}
