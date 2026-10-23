//! Known-answer tests for AES-128 and CFB8.
//!
//! Block vectors are FIPS-197 appendix B and C.1. The short CFB8 vector is
//! NIST SP 800-38A F.3.7. The 64-byte vector (key equal to the IV, as
//! Minecraft login uses) was produced with
//! `openssl enc -aes-128-cfb8 -K <hex> -iv <hex> -nopad -nosalt`.

use bytemill::crypto::{Aes128, Cfb8Decryptor, Cfb8Encryptor};

fn hex16(text: &str) -> [u8; 16] {
    let bytes = hex(text);
    bytes.try_into().expect("16-byte vector")
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn fips197_c1_encrypt_block() {
    let aes = Aes128::new(&hex16("000102030405060708090a0b0c0d0e0f"));
    let out = aes.encrypt_block(&hex16("00112233445566778899aabbccddeeff"));
    assert_eq!(out, hex16("69c4e0d86a7b0430d8cdb78070b4c55a"));
}

#[test]
fn fips197_appendix_b_encrypt_block() {
    let aes = Aes128::new(&hex16("2b7e151628aed2a6abf7158809cf4f3c"));
    let out = aes.encrypt_block(&hex16("3243f6a8885a308d313198a2e0370734"));
    assert_eq!(out, hex16("3925841d02dc09fbdc118597196a0b32"));
}

#[test]
fn sp800_38a_cfb8_aes128() {
    let key = hex16("2b7e151628aed2a6abf7158809cf4f3c");
    let iv = hex16("000102030405060708090a0b0c0d0e0f");
    let plaintext = hex("6bc1bee22e409f96e93d7e117393172aae2d");
    let ciphertext = hex("3b79424c9c0dd436bace9e0ed4586a4f32b9");

    let mut encrypted = plaintext.clone();
    Cfb8Encryptor::new(&key, &iv).encrypt(&mut encrypted);
    assert_eq!(encrypted, ciphertext);

    let mut decrypted = ciphertext;
    Cfb8Decryptor::new(&key, &iv).decrypt(&mut decrypted);
    assert_eq!(decrypted, plaintext);
}

#[test]
fn roundtrip_and_chunk_invariance() {
    let key = hex16("00112233445566778899aabbccddeeff");
    let iv = hex16("0f0e0d0c0b0a09080706050403020100");
    let plaintext = prng_bytes(1000);

    let mut whole = plaintext.clone();
    Cfb8Encryptor::new(&key, &iv).encrypt(&mut whole);

    let mut by_byte = plaintext.clone();
    let mut enc = Cfb8Encryptor::new(&key, &iv);
    for byte in &mut by_byte {
        enc.encrypt(std::slice::from_mut(byte));
    }
    assert_eq!(by_byte, whole);

    let mut uneven = plaintext.clone();
    encrypt_uneven(&key, &iv, &mut uneven);
    assert_eq!(uneven, whole);

    let mut recovered = whole.clone();
    Cfb8Decryptor::new(&key, &iv).decrypt(&mut recovered);
    assert_eq!(recovered, plaintext);

    let mut recovered_bytes = whole.clone();
    let mut dec = Cfb8Decryptor::new(&key, &iv);
    for byte in &mut recovered_bytes {
        dec.decrypt(std::slice::from_mut(byte));
    }
    assert_eq!(recovered_bytes, plaintext);

    let mut recovered_uneven = whole;
    decrypt_uneven(&key, &iv, &mut recovered_uneven);
    assert_eq!(recovered_uneven, plaintext);
}

#[test]
fn minecraft_style_key_equals_iv() {
    // openssl enc -aes-128-cfb8 -K 00112233445566778899aabbccddeeff
    //   -iv 00112233445566778899aabbccddeeff -nopad -nosalt
    // plaintext is bytes 0x00..=0x3f.
    let secret = hex16("00112233445566778899aabbccddeeff");
    let plaintext: Vec<u8> = (0..64).collect();
    let expected = hex("62b1e00b51105f34da28d079cec0b45b\
         406e9c83ab35f62479d77e3ef4d35a7e\
         8da34a0d52262f4a8cb30ae07df05f99\
         f12bf31dc642a0de0386977422d655f8");
    let mut encrypted = plaintext.clone();
    Cfb8Encryptor::new(&secret, &secret).encrypt(&mut encrypted);
    assert_eq!(encrypted, expected);

    let mut decrypted = encrypted;
    Cfb8Decryptor::new(&secret, &secret).decrypt(&mut decrypted);
    assert_eq!(decrypted, plaintext);
}

fn encrypt_uneven(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8]) {
    let mut enc = Cfb8Encryptor::new(key, iv);
    apply_uneven(&mut |chunk| enc.encrypt(chunk), data);
}

fn decrypt_uneven(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8]) {
    let mut dec = Cfb8Decryptor::new(key, iv);
    apply_uneven(&mut |chunk| dec.decrypt(chunk), data);
}

fn apply_uneven(step: &mut dyn FnMut(&mut [u8]), data: &mut [u8]) {
    let widths = [1usize, 2, 3, 5, 8, 13, 21];
    let mut offset = 0;
    let mut index = 0;
    while offset < data.len() {
        let end = (offset + widths[index % widths.len()]).min(data.len());
        step(&mut data[offset..end]);
        offset = end;
        index += 1;
    }
}

fn prng_bytes(len: usize) -> Vec<u8> {
    let mut state = 0x6a09_e667_u32;
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        out.push((state >> 24) as u8);
    }
    out
}
