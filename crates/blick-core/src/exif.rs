//! Minimaler TIFF/EXIF-Zugriff, nur für das Orientation-Tag (0x0112) in IFD0.
//!
//! Für mehr braucht Blick keine EXIF-Bibliothek: Gedreht wird bei JPEG,
//! indem nur dieses eine Tag umgeschrieben wird. Das ist verlustfrei und
//! kostet unabhängig von der Bildgröße ein paar Mikrosekunden.

use crate::rotate::Rotation;

const TAG_ORIENTATION: u16 = 0x0112;
const TYPE_SHORT: u16 = 3;

#[derive(Clone, Copy)]
enum Endian {
    Little,
    Big,
}

impl Endian {
    fn u16(self, b: &[u8]) -> u16 {
        match self {
            Endian::Little => u16::from_le_bytes([b[0], b[1]]),
            Endian::Big => u16::from_be_bytes([b[0], b[1]]),
        }
    }

    fn u32(self, b: &[u8]) -> u32 {
        match self {
            Endian::Little => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            Endian::Big => u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
        }
    }

    fn put_u16(self, b: &mut [u8], v: u16) {
        let bytes = match self {
            Endian::Little => v.to_le_bytes(),
            Endian::Big => v.to_be_bytes(),
        };
        b[..2].copy_from_slice(&bytes);
    }
}

/// Manche Container liefern den Block mit `Exif\0\0`-Präfix, andere ohne.
fn tiff_start(data: &[u8]) -> usize {
    if data.starts_with(b"Exif\0\0") { 6 } else { 0 }
}

/// Byte-Offset des Werts von Orientation, falls das Tag in IFD0 steht.
fn orientation_offset(data: &[u8]) -> Option<(Endian, usize)> {
    let base = tiff_start(data);
    let tiff = data.get(base..)?;
    let endian = match tiff.get(..2)? {
        b"II" => Endian::Little,
        b"MM" => Endian::Big,
        _ => return None,
    };
    if endian.u16(tiff.get(2..4)?) != 42 {
        return None;
    }
    let ifd = endian.u32(tiff.get(4..8)?) as usize;
    let count = endian.u16(tiff.get(ifd..ifd + 2)?) as usize;
    (0..count).find_map(|i| {
        let entry = ifd + 2 + i * 12;
        let e = tiff.get(entry..entry + 12)?;
        (endian.u16(&e[0..2]) == TAG_ORIENTATION && endian.u16(&e[2..4]) == TYPE_SHORT)
            .then_some((endian, base + entry + 8))
    })
}

pub fn read_orientation(data: &[u8]) -> Option<u16> {
    let (endian, at) = orientation_offset(data)?;
    let v = endian.u16(&data[at..at + 2]);
    (1..=8).contains(&v).then_some(v)
}

/// Schreibt den Wert an Ort und Stelle. `false`, wenn das Tag fehlt;
/// ein neues Tag einzufügen hieße, das IFD neu aufzubauen.
pub fn write_orientation(data: &mut [u8], value: u16) -> bool {
    match orientation_offset(data) {
        Some((endian, at)) => {
            endian.put_u16(&mut data[at..], value);
            true
        }
        None => false,
    }
}

/// Kleinster gültiger TIFF-Block mit nur einem Eintrag: Orientation.
pub fn minimal_exif(orientation: u16) -> Vec<u8> {
    let mut v = Vec::with_capacity(26);
    v.extend_from_slice(b"MM\0\x2a");
    v.extend_from_slice(&8u32.to_be_bytes());
    v.extend_from_slice(&1u16.to_be_bytes());
    v.extend_from_slice(&TAG_ORIENTATION.to_be_bytes());
    v.extend_from_slice(&TYPE_SHORT.to_be_bytes());
    v.extend_from_slice(&1u32.to_be_bytes());
    v.extend_from_slice(&orientation.to_be_bytes());
    v.extend_from_slice(&[0, 0]);
    v.extend_from_slice(&0u32.to_be_bytes());
    v
}

/// Neue Orientation nach einer Drehung um 90°.
///
/// Gedacht als (Spiegelung, Drehung im Uhrzeigersinn): 1/6/3/8 sind 0/90/180/270
/// ohne Spiegelung, 2/7/4/5 dieselben Winkel mit horizontaler Spiegelung zuerst.
pub fn rotate_orientation(o: u16, rot: Rotation) -> u16 {
    const CW: [u16; 9] = [0, 6, 7, 8, 5, 2, 3, 4, 1];
    const CCW: [u16; 9] = [0, 8, 5, 6, 7, 4, 1, 2, 3];
    let o = if (1..=8).contains(&o) { o as usize } else { 1 };
    match rot {
        Rotation::Right => CW[o],
        Rotation::Left => CCW[o],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_block_roundtrips() {
        let mut b = minimal_exif(1);
        assert_eq!(read_orientation(&b), Some(1));
        assert!(write_orientation(&mut b, 6));
        assert_eq!(read_orientation(&b), Some(6));
    }

    #[test]
    fn handles_exif_prefix() {
        let mut b = b"Exif\0\0".to_vec();
        b.extend(minimal_exif(3));
        assert_eq!(read_orientation(&b), Some(3));
    }

    #[test]
    fn garbage_is_none() {
        assert_eq!(read_orientation(b"nope"), None);
        assert_eq!(read_orientation(&[]), None);
        assert!(!write_orientation(&mut [0u8; 4], 1));
    }

    #[test]
    fn four_turns_are_identity_and_left_undoes_right() {
        for o in 1..=8 {
            let mut x = o;
            for _ in 0..4 {
                x = rotate_orientation(x, Rotation::Right);
            }
            assert_eq!(x, o);
            assert_eq!(
                rotate_orientation(rotate_orientation(o, Rotation::Right), Rotation::Left),
                o
            );
        }
        assert_eq!(rotate_orientation(1, Rotation::Right), 6);
        assert_eq!(rotate_orientation(1, Rotation::Left), 8);
    }
}
