use super::c_str;
use std::ffi::c_char;

#[test]
fn c_str_stops_at_nul_or_array_end() {
    let full = [b'a' as c_char; 4];
    assert_eq!(c_str(&full), "aaaa");
    let short = [b'h' as c_char, b'i' as c_char, 0, b'x' as c_char];
    assert_eq!(c_str(&short), "hi");
}
