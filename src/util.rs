pub(crate) fn fmt_num(v: f32) -> String {
    if v == 0.0 {
        return "0".to_string();
    }
    let s = format!("{:.8}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() {
        "0".to_string()
    } else {
        s.to_string()
    }
}

pub(crate) fn read16(b: &[u8], off: usize, little: bool) -> Option<u16> {
    let s = b.get(off..off + 2)?;
    Some(if little { u16::from_le_bytes([s[0], s[1]]) } else { u16::from_be_bytes([s[0], s[1]]) })
}

pub(crate) fn read32(b: &[u8], off: usize, little: bool) -> Option<u32> {
    let s = b.get(off..off + 4)?;
    Some(if little { u32::from_le_bytes([s[0], s[1], s[2], s[3]]) } else { u32::from_be_bytes([s[0], s[1], s[2], s[3]]) })
}
