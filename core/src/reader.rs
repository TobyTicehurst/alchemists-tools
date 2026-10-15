//! Little-endian byte reader with the UE string/name primitives.

use anyhow::{bail, Result};

pub struct Reader<'a> {
    pub data: &'a [u8],
    pub position: usize,
    pub names: &'a [String],
}

macro_rules! read_little_endian {
    ($($name:ident: $value_type:ty),*) => {$(
        pub fn $name(&mut self) -> Result<$value_type> {
            let bytes = self.take(std::mem::size_of::<$value_type>())?;
            Ok(<$value_type>::from_le_bytes(bytes.try_into().unwrap()))
        }
    )*};
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8], names: &'a [String]) -> Self {
        Reader {
            data,
            position: 0,
            names,
        }
    }

    pub fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        if self.position + length > self.data.len() {
            bail!(
                "read of {length} bytes at {:#x} runs past the end ({:#x})",
                self.position,
                self.data.len()
            );
        }
        let bytes = &self.data[self.position..self.position + length];
        self.position += length;
        Ok(bytes)
    }

    read_little_endian!(
        u8: u8, i8: i8, u16: u16, i16: i16, u32: u32,
        i32: i32, u64: u64, i64: i64, f32: f32, f64: f64
    );

    /// FString: i32 length (negative = UTF-16), including the terminating NUL.
    pub fn fstring(&mut self) -> Result<String> {
        let length = self.i32()?;
        let string = if length < 0 {
            let bytes = self.take(2 * length.unsigned_abs() as usize)?;
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        } else {
            String::from_utf8_lossy(self.take(length as usize)?).into_owned()
        };
        Ok(string.trim_end_matches('\0').to_string())
    }

    /// FName: index into the package name map plus an instance number (`Name_3` is stored as 4).
    pub fn fname(&mut self) -> Result<String> {
        let (index, number) = (self.i32()?, self.i32()?);
        let Some(name) = usize::try_from(index).ok().and_then(|i| self.names.get(i)) else {
            bail!(
                "name index {index} out of range at {:#x}",
                self.position - 8
            );
        };
        Ok(if number != 0 {
            format!("{name}_{}", number - 1)
        } else {
            name.clone()
        })
    }

    /// Unversioned property header: the indices of the serialized properties and whether each is
    /// zero.
    pub fn unversioned_header(&mut self) -> Result<Vec<(usize, bool)>> {
        let mut fragments = vec![];
        let mut total = 0;
        loop {
            let packed = self.u16()?;
            let (skip, has_zeroes, is_last, count) = (
                (packed & 0x7F) as usize,
                packed & 0x80 != 0,
                packed & 0x100 != 0,
                (packed >> 9) as usize,
            );
            fragments.push((skip, has_zeroes, count));
            total += count;
            if is_last {
                break;
            }
        }
        let mut zero_mask = vec![false; total];
        if fragments.iter().any(|(_, has_zeroes, _)| *has_zeroes) {
            let mask_bytes = match total {
                0..=8 => 1,
                9..=16 => 2,
                _ => total.div_ceil(32) * 4,
            };
            let mask = self.take(mask_bytes)?;
            for (bit, is_zero) in zero_mask.iter_mut().enumerate() {
                *is_zero = mask[bit / 8] >> (bit % 8) & 1 != 0;
            }
        }
        let mut present = Vec::with_capacity(total);
        let (mut index, mut bit) = (0, 0);
        for (skip, _, count) in fragments {
            index += skip;
            for _ in 0..count {
                present.push((index, zero_mask[bit]));
                index += 1;
                bit += 1;
            }
        }
        Ok(present)
    }
}

/// The parts of a legacy `.uasset` header the decoders need.
pub struct Package {
    pub names: Vec<String>,
    /// Object name of each import (import `-1` is `imports[0]`).
    pub imports: Vec<String>,
    /// Object name of each export (export `1` is `exports[0]`).
    pub exports: Vec<String>,
    /// Class name of each export.
    pub export_classes: Vec<String>,
    /// Full object path of each import, e.g.
    /// `/Game/Arts/UI/Ingredients/T_UI_PortalSigil.T_UI_PortalSigil`.
    pub import_paths: Vec<String>,
    /// Full object path of each export, e.g. `/Game/DataTables/DT_Enemies.DT_Enemies`.
    pub export_paths: Vec<String>,
}

impl Package {
    pub fn read(uasset: &[u8]) -> Result<Self> {
        let header = retoc::legacy_asset::FLegacyPackageHeader::deserialize(
            &mut std::io::Cursor::new(uasset),
            Some(crate::unpack::package_version()),
        )?;
        let name = |minimal_name| {
            header
                .name_map
                .get(minimal_name)
                .map(|name| name.into_owned())
        };
        let imports: Vec<String> = header
            .imports
            .iter()
            .map(|import| name(import.object_name))
            .collect::<Result<_>>()?;
        let exports: Vec<String> = header
            .exports
            .iter()
            .map(|export| name(export.object_name))
            .collect::<Result<_>>()?;
        let import_outers: Vec<i32> = header
            .imports
            .iter()
            .map(|import| import.outer_index.index)
            .collect();
        let export_outers: Vec<i32> = header
            .exports
            .iter()
            .map(|export| export.outer_index.index)
            .collect();

        // An object's path is its chain of outers: an import's ends at a package import, an
        // export's at this package.
        let package_name = &header.summary.package_name;
        let object_path = |index: i32| {
            let mut chain = vec![];
            let mut current = index;
            while current != 0 && chain.len() < 64 {
                let (names, outers) = if current < 0 {
                    (&imports, &import_outers)
                } else {
                    (&exports, &export_outers)
                };
                let i = current.unsigned_abs() as usize - 1;
                let (Some(name), Some(outer)) = (names.get(i), outers.get(i)) else {
                    break;
                };
                chain.push(name.clone());
                if current > 0 && *outer == 0 {
                    chain.push(package_name.clone());
                }
                current = *outer;
            }
            chain.reverse();
            join_object_path(&chain)
        };
        let import_paths = (1..=imports.len() as i32)
            .map(|i| object_path(-i))
            .collect();
        let export_paths = (1..=exports.len() as i32).map(object_path).collect();

        let mut package = Package {
            names: header.name_map.copy_raw_names(),
            imports,
            exports,
            export_classes: vec![],
            import_paths,
            export_paths,
        };
        package.export_classes = header
            .exports
            .iter()
            .map(|export| {
                package
                    .object(export.class_index.index)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        Ok(package)
    }

    /// The name of the object a package index refers to: negative for imports, positive for
    /// exports.
    pub fn object(&self, index: i32) -> Option<&str> {
        let list = if index < 0 {
            &self.imports
        } else {
            &self.exports
        };
        let i = usize::try_from(index.unsigned_abs()).ok()?.checked_sub(1)?;
        list.get(i).map(String::as_str)
    }

    /// The full object path a package index refers to, e.g.
    /// `/Game/Arts/UI/Ingredients/T_UI_PortalSigil.T_UI_PortalSigil`.
    pub fn object_path(&self, index: i32) -> Option<&str> {
        let list = if index < 0 {
            &self.import_paths
        } else {
            &self.export_paths
        };
        let i = usize::try_from(index.unsigned_abs()).ok()?.checked_sub(1)?;
        list.get(i).map(String::as_str)
    }
}

/// Unreal's object path form: `/Game/Package.Asset:SubObject.Nested`.
fn join_object_path(chain: &[String]) -> String {
    let mut path = String::new();
    for (i, part) in chain.iter().enumerate() {
        match i {
            0 => {}
            1 => path.push('.'),
            2 => path.push(':'),
            _ => path.push('.'),
        }
        path.push_str(part);
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_with_zero_mask() {
        // one fragment: skip 1, has zeroes, last, 3 properties; mask 0b010
        let packed: u16 = 1 | 0x80 | 0x100 | (3 << 9);
        let mut data = packed.to_le_bytes().to_vec();
        data.push(0b010);
        let mut reader = Reader::new(&data, &[]);
        assert_eq!(
            reader.unversioned_header().unwrap(),
            vec![(1, false), (2, true), (3, false)]
        );
        assert_eq!(reader.position, data.len());
    }

    #[test]
    fn fstrings() {
        let mut data = vec![];
        data.extend(3i32.to_le_bytes());
        data.extend(b"ab\0");
        data.extend((-2i32).to_le_bytes());
        data.extend([0x41, 0x6c, 0, 0]); // U+6C41 + NUL as UTF-16
        let mut reader = Reader::new(&data, &[]);
        assert_eq!(reader.fstring().unwrap(), "ab");
        assert_eq!(reader.fstring().unwrap(), "\u{6c41}");
    }

    #[test]
    fn object_paths() {
        let path = |parts: &[&str]| {
            join_object_path(
                &parts
                    .iter()
                    .map(|part| part.to_string())
                    .collect::<Vec<_>>(),
            )
        };
        assert_eq!(path(&["/Script/Engine"]), "/Script/Engine");
        assert_eq!(
            path(&["/Game/UI/T_Icon", "T_Icon"]),
            "/Game/UI/T_Icon.T_Icon"
        );
        assert_eq!(
            path(&["/Game/Map", "Map", "Actor", "Component"]),
            "/Game/Map.Map:Actor.Component"
        );
    }
}
