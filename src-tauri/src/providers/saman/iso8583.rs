//! ISO-8583:1987 codec for the SSP1126 dialect.
//!
//! Faithful port of `iso8583.ts` from the `saman-payment-pos` npm library:
//!  - N / XN  -> BCD packed nibbles ("ASCHEX"), left zero-padded
//!  - A/AN/ANS/... -> ASCII (windows-1256), fixed = right space-padded
//!  - B/NS/Z  -> raw binary
//!  - LLVAR   -> 1 BCD length byte, LLLVAR/LLLLVAR -> 2 BCD length bytes
//!  - bitmap  -> standard primary (+ secondary for fields > 64)
//!
//! Plus the DE64 DES-CBC MAC (fixed shared key from the original vendor source).

use std::collections::BTreeMap;
use std::sync::OnceLock;

use cbc::Encryptor;
use cipher::block_padding::NoPadding;
use cipher::{BlockEncryptMut, KeyIvInit};
use des::Des;

pub const MAC_KEY: [u8; 8] = [0x23, 0xAB, 0xE1, 0x82, 0xCA, 0xB5, 0x64, 0x7D];
pub const MAC_IV: [u8; 8] = [0; 8];

#[derive(Clone, Copy, PartialEq)]
enum Ft {
    N,
    Ns,
    Xn,
    An,
    Ans,
    B,
    Z,
    Bmp,
}

#[derive(Clone, Copy, PartialEq)]
enum Vl {
    Fixed,
    LlVar,
    LllVar,
}

#[derive(Clone, Copy)]
struct FieldDef {
    ftype: Ft,
    len: usize,
    var_len: Vl,
}

const fn f(ftype: Ft, len: usize, var_len: Vl) -> FieldDef {
    FieldDef { ftype, len, var_len }
}

const MAX_FIELD: u16 = 128;

/// ISO-8583:1987 field table (from `dl_iso8583_defs_1987.pas`).
fn defs() -> &'static [FieldDef; 129] {
    static DEFS: OnceLock<[FieldDef; 129]> = OnceLock::new();
    DEFS.get_or_init(|| {
        use Ft::*;
        use Vl::*;
        let mut d = [f(Ans, 999, LllVar); 129];
        let table: [FieldDef; 105] = [
            f(N, 4, Fixed),        // 0 MTI
            f(Bmp, 16, Fixed),     // 1 bitmap
            f(N, 19, LlVar),       // 2 PAN
            f(N, 6, Fixed),        // 3 processing code
            f(N, 12, Fixed),       // 4 amount, txn
            f(N, 12, Fixed),       // 5
            f(N, 12, Fixed),       // 6 amount, billing
            f(N, 10, Fixed),       // 7 transmission date/time
            f(N, 8, Fixed),        // 8
            f(N, 8, Fixed),        // 9
            f(N, 8, Fixed),        // 10
            f(N, 6, Fixed),        // 11 STAN
            f(N, 6, Fixed),        // 12 local time HHMMSS
            f(N, 4, Fixed),        // 13 local date MMDD
            f(N, 4, Fixed),        // 14
            f(N, 6, Fixed),        // 15
            f(N, 4, Fixed),        // 16
            f(N, 4, Fixed),        // 17
            f(N, 4, Fixed),        // 18
            f(N, 3, Fixed),        // 19
            f(N, 3, Fixed),        // 20
            f(N, 3, Fixed),        // 21
            f(N, 3, Fixed),        // 22
            f(N, 3, Fixed),        // 23
            f(N, 3, Fixed),        // 24 NII
            f(N, 2, Fixed),        // 25 POS condition code
            f(N, 2, Fixed),        // 26
            f(N, 1, Fixed),        // 27
            f(Xn, 9, Fixed),       // 28
            f(Xn, 9, Fixed),       // 29
            f(Xn, 9, Fixed),       // 30
            f(Xn, 9, Fixed),       // 31
            f(N, 11, LlVar),       // 32
            f(N, 11, LlVar),       // 33
            f(Ns, 28, LlVar),      // 34
            f(Z, 37, LlVar),       // 35 track2
            f(An, 104, LllVar),    // 36 track3
            f(An, 12, Fixed),      // 37 RRN
            f(An, 6, Fixed),       // 38 approval
            f(An, 2, Fixed),       // 39 response code
            f(Ans, 3, Fixed),      // 40
            f(Ans, 8, Fixed),      // 41 terminal id
            f(Ans, 15, Fixed),     // 42
            f(Ans, 40, Fixed),     // 43
            f(Ans, 25, LlVar),     // 44
            f(Ans, 76, LlVar),     // 45 track1
            f(Ans, 999, LllVar),   // 46 additional data - ISO
            f(Ans, 999, LllVar),   // 47 additional data - national
            f(Ans, 999, LllVar),   // 48 additional data - private
            f(Ans, 3, Fixed),      // 49 currency
            f(An, 3, Fixed),       // 50
            f(An, 3, Fixed),       // 51
            f(B, 8, Fixed),        // 52 PIN
            f(N, 16, Fixed),       // 53
            f(Ans, 120, LllVar),   // 54 additional amounts
            f(Ans, 999, LllVar),   // 55
            f(Ans, 999, LllVar),   // 56
            f(Ans, 999, LllVar),   // 57
            f(Ans, 999, LllVar),   // 58
            f(Ans, 999, LllVar),   // 59
            f(Ans, 999, LllVar),   // 60
            f(Ans, 999, LllVar),   // 61
            f(Ans, 999, LllVar),   // 62
            f(Ans, 999, LllVar),   // 63
            f(B, 8, Fixed),        // 64 MAC
            f(B, 8, Fixed),        // 65
            f(N, 1, Fixed),        // 66
            f(N, 2, Fixed),        // 67
            f(N, 3, Fixed),        // 68
            f(N, 3, Fixed),        // 69
            f(N, 3, Fixed),        // 70
            f(N, 4, Fixed),        // 71
            f(N, 4, Fixed),        // 72
            f(N, 6, Fixed),        // 73
            f(N, 10, Fixed),       // 74
            f(N, 10, Fixed),       // 75
            f(N, 10, Fixed),       // 76
            f(N, 10, Fixed),       // 77
            f(N, 10, Fixed),       // 78
            f(N, 10, Fixed),       // 79
            f(N, 10, Fixed),       // 80
            f(N, 10, Fixed),       // 81
            f(N, 12, Fixed),       // 82
            f(N, 12, Fixed),       // 83
            f(N, 12, Fixed),       // 84
            f(N, 12, Fixed),       // 85
            f(N, 15, Fixed),       // 86
            f(N, 15, Fixed),       // 87
            f(N, 15, Fixed),       // 88
            f(N, 15, Fixed),       // 89
            f(N, 42, Fixed),       // 90
            f(Ans, 1, Fixed),      // 91
            f(N, 2, Fixed),        // 92
            f(N, 5, Fixed),        // 93
            f(Ans, 7, Fixed),      // 94
            f(Ans, 42, Fixed),     // 95
            f(B, 8, Fixed),        // 96
            f(Xn, 17, Fixed),      // 97
            f(Ans, 25, Fixed),     // 98
            f(N, 11, LlVar),       // 99
            f(N, 11, LlVar),       // 100
            f(Ans, 17, LlVar),     // 101
            f(Ans, 28, LlVar),     // 102
            f(Ans, 28, LlVar),     // 103
            f(Ans, 100, LllVar),   // 104
        ];
        d[..105].copy_from_slice(&table);
        // 105..127 stay reserved ANS LLLVAR; 128 is the (unused here) B8 MAC slot
        d[128] = f(B, 8, Fixed);
        d
    })
}

const SP: u8 = 0x20;

fn bcd_byte(v: usize) -> u8 {
    ((((v / 10) % 10) << 4) | (v % 10)) as u8
}

fn nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'A'..=b'F' => c - 55,
        b'a'..=b'f' => c - 87,
        _ => 0,
    }
}

// ------------------------------------------------------------------ text codec
//
// The terminal speaks windows-1256 (Arabic/Persian). Below 0x80 it is
// byte-identical to ASCII. Unmappable characters become '?' — the conventional
// substitution, matching the reference library.

fn cp1256_to_char(b: u8) -> char {
    let bytes = [b];
    let (s, _, _) = encoding_rs::WINDOWS_1256.decode(&bytes);
    s.chars().next().unwrap_or('\u{fffd}')
}

fn char_to_cp1256() -> &'static std::collections::HashMap<char, u8> {
    static MAP: OnceLock<std::collections::HashMap<char, u8>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = std::collections::HashMap::new();
        for b in 0u16..=255 {
            let ch = cp1256_to_char(b as u8);
            m.entry(ch).or_insert(b as u8);
        }
        m
    })
}

pub fn encode_text(value: &str) -> Vec<u8> {
    let map = char_to_cp1256();
    value
        .chars()
        .map(|ch| *map.get(&ch).unwrap_or(&0x3f))
        .collect()
}

/// Decode windows-1256 bytes, stripping the NUL padding the terminal appends to
/// some variable-length text fields.
pub fn decode_text(b: &[u8]) -> String {
    let mut end = b.len();
    while end > 0 && b[end - 1] == 0x00 {
        end -= 1;
    }
    let (s, _, _) = encoding_rs::WINDOWS_1256.decode(&b[..end]);
    s.into_owned()
}

// ------------------------------------------------------------------- message

#[derive(Debug, thiserror::Error)]
pub enum IsoError {
    #[error("field {0}: value too long")]
    ValueTooLong(u16),
    #[error("truncated message while reading field {0}")]
    Truncated(u16),
}

/// Packed ISO-8583 message. Field values are stored as their logical "content bytes".
#[derive(Default, Clone)]
pub struct Iso8583Message {
    fields: BTreeMap<u16, Vec<u8>>,
}

impl Iso8583Message {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_str(&mut self, field: u16, value: &str) -> &mut Self {
        self.fields.insert(field, encode_text(value));
        self
    }

    pub fn set_bin(&mut self, field: u16, value: &[u8]) -> &mut Self {
        self.fields.insert(field, value.to_vec());
        self
    }

    pub fn has(&self, field: u16) -> bool {
        self.fields.contains_key(&field)
    }

    pub fn get_str(&self, field: u16) -> Option<String> {
        self.fields.get(&field).map(|b| decode_text(b))
    }

    pub fn get_bin(&self, field: u16) -> Option<Vec<u8>> {
        self.fields.get(&field).cloned()
    }

    pub fn set_mti(&mut self, mti: &str) -> &mut Self {
        self.set_str(0, mti)
    }

    pub fn mti(&self) -> Option<String> {
        self.get_str(0)
    }

    fn pack_bitmap(&self, w: &mut Vec<u8>) {
        let secondary = (65..=MAX_FIELD).any(|n| self.fields.contains_key(&n));
        let mut bytes = vec![0u8; if secondary { 16 } else { 8 }];
        if secondary {
            bytes[0] |= 0x80; // bit 1 = secondary present
        }
        let top = if secondary { MAX_FIELD } else { 64 };
        for n in 2..=top {
            if self.fields.contains_key(&n) {
                let idx = ((n - 1) >> 3) as usize;
                let bit = ((n - 1) & 7) as u32;
                bytes[idx] |= 0x80u8 >> bit;
            }
        }
        w.extend_from_slice(&bytes);
    }

    fn put_var_len(w: &mut Vec<u8>, var_len: Vl, act_len: usize) {
        match var_len {
            Vl::Fixed => {}
            Vl::LlVar => {
                let v = act_len % 100;
                w.push(bcd_byte(v));
            }
            Vl::LllVar => {
                let v = act_len % 1000;
                w.push(bcd_byte(v / 100));
                w.push(bcd_byte(v % 100));
            }
        }
    }

    fn pack_field(&self, w: &mut Vec<u8>, field: u16, def: &FieldDef) -> Result<(), IsoError> {
        let content = self.fields.get(&field).expect("field present");
        let act_len = content.len();
        let req_len = if def.var_len == Vl::Fixed { def.len } else { act_len };

        if def.ftype == Ft::N || def.ftype == Ft::Xn {
            // ASCHEX: content is ASCII digits, packed 2-per-byte (BCD nibbles)
            Self::put_var_len(w, def.var_len, act_len);
            if act_len > req_len {
                return Err(IsoError::ValueTooLong(field));
            }
            let whole_req_bytes = (req_len + 1) >> 1;
            let target_digits = whole_req_bytes * 2;
            let mut digits: Vec<u8> = Vec::with_capacity(target_digits);
            for _ in 0..target_digits.saturating_sub(act_len) {
                digits.push(b'0');
            }
            digits.extend_from_slice(content);
            for pair in digits.chunks(2) {
                w.push((nibble(pair[0]) << 4) | nibble(pair[1]));
            }
            return Ok(());
        }

        Self::put_var_len(w, def.var_len, act_len);
        if act_len > req_len {
            return Err(IsoError::ValueTooLong(field));
        }
        w.extend_from_slice(content);
        let pad = if def.ftype == Ft::B || def.ftype == Ft::Ns || def.ftype == Ft::Z {
            0u8
        } else {
            SP
        };
        for _ in act_len..req_len {
            w.push(pad);
        }
        Ok(())
    }

    /// Pack without touching the MAC (DE64 used as-is if present).
    pub fn pack(&self) -> Result<Vec<u8>, IsoError> {
        let table = defs();
        let mut w = Vec::with_capacity(64);
        for field in 0..=MAX_FIELD {
            let def = &table[field as usize];
            if def.ftype == Ft::Bmp {
                self.pack_bitmap(&mut w);
            } else if self.fields.contains_key(&field) {
                self.pack_field(&mut w, field, def)?;
            }
        }
        Ok(w)
    }

    /// Pack and append the SSP1126 DE64 CBC-MAC. Returns the full on-wire message.
    pub fn pack_with_mac(&mut self) -> Result<Vec<u8>, IsoError> {
        self.set_bin(64, &[0u8; 8]); // placeholder so the bitmap reserves DE64
        let mut packed = self.pack()?;
        let body_len = packed.len() - 8;
        let mac = compute_mac(&packed[..body_len]);
        packed[body_len..].copy_from_slice(&mac);
        Ok(packed)
    }

    /// Parse a raw ISO-8583 message (MTI..fields..MAC).
    pub fn unpack(buf: &[u8]) -> Result<Iso8583Message, IsoError> {
        let table = defs();
        let mut msg = Iso8583Message::new();
        let mut off = 0usize;
        let mut present = [false; (MAX_FIELD + 1) as usize];

        fn read_field(
            msg: &mut Iso8583Message,
            buf: &[u8],
            off: &mut usize,
            field: u16,
            def: &FieldDef,
        ) -> Result<(), IsoError> {
            let mut size = def.len;
            if def.var_len != Vl::Fixed {
                let digits = match def.var_len {
                    Vl::LlVar => 2usize,
                    Vl::LllVar => 4usize,
                    Vl::Fixed => unreachable!(),
                };
                let mut v: usize = 0;
                let mut n = digits;
                while n > 0 {
                    let byte = *buf.get(*off).ok_or(IsoError::Truncated(field))?;
                    *off += 1;
                    v = v * 100 + (((byte >> 4) & 0xf) as usize) * 10 + ((byte & 0xf) as usize);
                    n -= 2;
                }
                size = def.len.min(v);
            }
            if def.ftype == Ft::N || def.ftype == Ft::Xn {
                // size = number of digits; bytes = ceil(size/2); leading nibble is pad if odd
                let mut digits = String::new();
                let mut remaining = size;
                if remaining % 2 == 1 {
                    let byte = *buf.get(*off).ok_or(IsoError::Truncated(field))?;
                    digits.push(
                        char::from_digit((byte & 0x0f) as u32, 16)
                            .unwrap_or('0')
                            .to_ascii_uppercase(),
                    );
                    *off += 1;
                    remaining -= 1;
                }
                for _ in 0..remaining / 2 {
                    let byte = *buf.get(*off).ok_or(IsoError::Truncated(field))?;
                    *off += 1;
                    digits.push(
                        char::from_digit(((byte >> 4) & 0xf) as u32, 16)
                            .unwrap_or('0')
                            .to_ascii_uppercase(),
                    );
                    digits.push(
                        char::from_digit((byte & 0xf) as u32, 16)
                            .unwrap_or('0')
                            .to_ascii_uppercase(),
                    );
                }
                msg.set_str(field, &digits);
                return Ok(());
            }
            if *off + size > buf.len() {
                return Err(IsoError::Truncated(field));
            }
            let slice = &buf[*off..*off + size];
            *off += size;
            msg.set_bin(field, slice);
            Ok(())
        }

        // MTI
        read_field(&mut msg, buf, &mut off, 0, &table[0])?;

        // primary bitmap: bit index b (0 = MSB of byte 0) -> field (b+1);
        // field 1 (b=0) is the "secondary bitmap present" flag.
        if off + 8 > buf.len() {
            return Err(IsoError::Truncated(1));
        }
        let primary: [u8; 8] = buf[off..off + 8].try_into().unwrap();
        off += 8;
        for b in 1..64usize {
            let mask = 0x80u8 >> (b & 7);
            if primary[b >> 3] & mask != 0 {
                present[b + 1] = true; // field 2..64
            }
        }
        if primary[0] & 0x80 != 0 {
            if off + 8 > buf.len() {
                return Err(IsoError::Truncated(65));
            }
            let seg: [u8; 8] = buf[off..off + 8].try_into().unwrap();
            off += 8;
            for b in 0..64usize {
                let mask = 0x80u8 >> (b & 7);
                if seg[b >> 3] & mask != 0 {
                    let field = 65 + b;
                    if field <= MAX_FIELD as usize {
                        present[field] = true;
                    }
                }
            }
        }
        for field in 2..=MAX_FIELD {
            if present[field as usize] {
                read_field(&mut msg, buf, &mut off, field, &table[field as usize])?;
            }
        }
        Ok(msg)
    }
}

// ---------------------------------------------------------------------- MAC

/// SSP1126 CBC-MAC over an already-packed message body (without DE64):
/// zero-pad to a multiple of 8, single-DES CBC with the fixed key/zero IV,
/// MAC = last ciphertext block.
pub fn compute_mac(body: &[u8]) -> [u8; 8] {
    let pad_len = if body.len() % 8 == 0 { 0 } else { 8 - body.len() % 8 };
    let mut padded = body.to_vec();
    padded.extend(std::iter::repeat(0u8).take(pad_len));
    let enc = Encryptor::<Des>::new(&MAC_KEY.into(), &MAC_IV.into());
    let ct = enc
        .encrypt_padded_vec_mut::<NoPadding>(&padded);
    let mut mac = [0u8; 8];
    mac.copy_from_slice(&ct[ct.len() - 8..]);
    mac
}

/// Validate an incoming ISO message's DE64 MAC: strip last 8 bytes, recompute, compare.
pub fn verify_mac(raw_message: &[u8]) -> bool {
    if raw_message.len() < 8 {
        return false;
    }
    let (body, recv) = raw_message.split_at(raw_message.len() - 8);
    compute_mac(body) == recv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_roundtrip() {
        let mut m = Iso8583Message::new();
        m.set_mti("0300")
            .set_str(3, "410000")
            .set_str(12, "123456")
            .set_str(13, "0805")
            .set_str(25, "14")
            .set_str(49, "364");
        let raw = m.pack_with_mac().unwrap();
        assert!(verify_mac(&raw));
        let back = Iso8583Message::unpack(&raw).unwrap();
        assert_eq!(back.mti().as_deref(), Some("0300"));
        assert_eq!(back.get_str(3).as_deref(), Some("410000"));
        assert_eq!(back.get_str(12).as_deref(), Some("123456"));
        assert_eq!(back.get_str(13).as_deref(), Some("0805"));
        assert_eq!(back.get_str(25).as_deref(), Some("14"));
        assert_eq!(back.get_str(49).as_deref(), Some("364"));
        assert!(back.get_bin(64).is_some());
    }

    #[test]
    fn secondary_bitmap_roundtrip() {
        let mut m = Iso8583Message::new();
        m.set_mti("0300").set_str(3, "300000").set_str(66, "1");
        let raw = m.pack().unwrap();
        let back = Iso8583Message::unpack(&raw).unwrap();
        assert_eq!(back.get_str(66).as_deref(), Some("1"));
    }

    #[test]
    fn persian_text_roundtrip() {
        // Arabic-yeh spelling (U+064A), as the terminal's cp1256 encodes it —
        // windows-1256 has no byte for Farsi yeh U+06CC (verified against Node's
        // TextDecoder: bytes CE D1 ED CF ↔ خ ر ي د).
        let s = "خريد";
        let enc = encode_text(s);
        assert_eq!(enc, [0xCE, 0xD1, 0xED, 0xCF]);
        assert_eq!(decode_text(&enc), s);
    }

    #[test]
    fn nul_padding_stripped() {
        let mut b = encode_text("hash");
        b.extend_from_slice(&[0, 0, 0]);
        assert_eq!(decode_text(&b), "hash");
    }

    #[test]
    fn odd_length_numeric_field() {
        // DE2 PAN is N..19 LLVAR — odd digit counts get a leading pad nibble
        let mut m = Iso8583Message::new();
        m.set_mti("0300").set_str(2, "12345");
        let raw = m.pack().unwrap();
        let back = Iso8583Message::unpack(&raw).unwrap();
        assert_eq!(back.get_str(2).as_deref(), Some("12345"));
    }
}
