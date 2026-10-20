//! The frame shell's C ABI from include/frame_abi.h: types, and declarations of the functions the PC host library
//! defines (checked against them where they are defined).
#![allow(
    non_camel_case_types,
    non_upper_case_globals,
    clippy::missing_safety_doc
)]

include!(concat!(env!("OUT_DIR"), "/frame_abi.rs"));

/// Text of a fixed C string field
pub fn c_string<const N: usize>(field: &[std::ffi::c_char; N]) -> String {
    let bytes: Vec<u8> = field
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}
