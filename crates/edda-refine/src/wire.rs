//! Wire-format byte helpers shared across canonical encoding and certificate
//! serialisation. Locked by `distribution/03-certificate.md §4`.

/// Unsigned LEB128 varint encoder. Matches the wire spec in
/// `distribution/03-certificate.md §4` — the high bit signals continuation;
/// no single-byte encoding produces `0xFF` (which would collide with the
/// canonical-context separator byte).
pub(crate) fn encode_varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
            out.push(byte);
        } else {
            out.push(byte);
            return;
        }
    }
}
