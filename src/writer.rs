use bevy::utils::hashbrown::HashMap;
use bevy::utils::HashMap;
use bytemuck::{bytes_of_mut, cast_slice_mut, Pod, Zeroable};
use glam::IVec3;
use rand::Rng;
use std::collections::BTreeSet;
use std::io::{Seek, SeekFrom, Write};

use crate::data_structure::{
    ArchiveHeader, Compression, Grid, GridDescriptor, Metadata, MetadataValue, Node, Node3, Node4,
    Node5, NodeHeader, NodeMetaData, Tree,
};

const OPENVDB_MAJOR_VERSION: u32 = 11;
const OPENVDB_MINOR_VERSION: u32 = 0;
const OPENVDB_PATCH_VERSION: u32 = 1;
const OPENVDB_FILE_VERSION: u32 = 224;

#[derive(thiserror::Error, Debug)]
pub enum WriteError {
    #[error("Placeholder error until I finish all stuff")]
    PlaceHolderError,
}

pub struct VdbWriter<W: Write + Seek> {
    writer: W,
    uuid: [char; 16 * 2 + 4 + 1],
}

impl<W: Write + Seek> VdbWriter<W> {
    pub fn new(&mut writer: W, is_seekeable: bool) -> Result<Self, WriteError> {
        // 1) Write the magic number for VDB
        const MAGIC: u64 = 0x2042445600000000;
        writer.write(&MAGIC.to_le_bytes()).unwrap();

        // 2) Write the file format version number.
        writer.write(&OPENVDB_FILE_VERSION.to_le_bytes()).unwrap();

        // 3) Write the library version numbers.
        writer.write(&OPENVDB_MAJOR_VERSION.to_le_bytes()).unwrap();
        writer.write(&OPENVDB_MINOR_VERSION.to_le_bytes()).unwrap();

        // 4) Write a flag indicating that this stream contains no grid offsets.
        let is_seekeable_byte = if is_seekeable { 1u8 } else { 0u8 };
        writer.write(&is_seekeable_byte.to_le_bytes()).unwrap();

        // 5) Write a flag indicating that this stream contains compressed leaf data.
        //    (Omitted as of version 222)

        // 6) Generate a new random 16-byte (128-bit) sequence and write it to the stream.
        let mut rng = rand::thread_rng();

        let mut uuid_str = ['0'; 16 * 2 + 4 + 1];
        fn to_hex(c: u32) -> char {
            let c = c & 0xf;
            if c < 10 {
                ('0' as u8 + c as u8) as char
            } else {
                ((c - 10) as u8 + ('A' as u8)) as char
            }
        }

        for i in 0..4 {
            let mut v: u32 = rng.gen();
            // This writes out in reverse direction of bit order, but
            // as source is random we don't mind.
            for j in 0..8 {
                uuid_str[i * 8 + j] = to_hex(v);
                v >>= 4;
            }
        }

        // Insert our hyphens.
        for i in 0..4 {
            uuid_str[16 * 2 + i] = '-';
        }
        uuid_str.swap(16 * 2 + 0, 8 + 0);
        uuid_str.swap(16 * 2 + 1, 12 + 1);
        uuid_str.swap(16 * 2 + 2, 16 + 2);
        uuid_str.swap(16 * 2 + 3, 20 + 3);
        uuid_str[16 * 2 + 4] = 0 as char;

        let uuid = uuid_str;
        // We don't write a string; but instead a fixed length buffer.
        // To match the old UUID, we need an extra 4 bytes for hyphens.
        for i in 0..(16 * 2 + 4) {
            writer.write(&[uuid_str[i] as u8]).unwrap();
        }

        Ok(Self { writer, uuid })
    }
    pub fn write<ExpectedTy: Pod, ValueTy>(
        &self,
        grids: Vec<Grid<ExpectedTy>>,
        metadata: Metadata,
    ) -> bool {
        // Header is already written at this point
        let metadata_seek = self.writer.seek(SeekFrom::Current(0));
        Self::write_metadata(&mut self.writer, metadata);

        // Grid count (not sure this is right since they check the pointers in C++)
        self.writer.write(&grids.len().to_le_bytes());

        let mut tree_map: HashMap<Tree<ValueTy>, GridDescriptor>;

        // Determine which grid names are unique and which are not.
        let mut name_count: HashMap<String, u32 /* count */>;
        for g in grids.iter() {
            let g_name = g.descriptor.name;
            if name_count.get(&g_name).is_some() {
                name_count[&g_name] += 1;
            } else {
                name_count.insert(g_name, 1);
            }
        }

        let mut unique_names: BTreeSet<String> = BTreeSet::new();

        // Write all the non-null grid
        for g in grids.iter() {
            // Ensure that the grid's descriptor has a unique grid name, by appending
            // a number to it if a grid with the same name was already written.
            // Always add a number if the grid name is empty, so that the grid can be
            // properly identified as an instance parent, if necessary.

            let mut name = g.descriptor.name;
            if name.is_empty() || name_count[&name] > 1 {
                name = GridDescriptor::add_suffix(name, 0);
            }

            let mut n = 1;
            while unique_names.contains(&name) {
                name = GridDescriptor::add_suffix(g.descriptor.name, n);
            }
            unique_names.insert(name);

            // Create a new decriptor
            let mut gd = GridDescriptor {
                name,
                file_version: OPENVDB_FILE_VERSION,
                instance_parent: String::new(),
                grid_type: g.descriptor.grid_type,
                grid_pos: 0,
                block_pos: 0,
                end_pos: 0,
                compression: Compression::default(),
                meta_data: Metadata::default(),
            };
            // If original one is...
            if g.descriptor.meta_data.is_half_float() {
                gd.meta_data.0.insert(
                    String::from("is_saved_as_half_float"),
                    MetadataValue::Bool(true),
                );
            }

            // Check if this grid's tree is shared with a grid that has already been written.
            tree_map.values()
        }

        true
    }

    fn write_metadata(writer: &mut W, metadata: Metadata) {
        // Metadata count
        writer.write(&metadata.0.len().to_le_bytes());

        //for i in 0..metadata.0.len() {
        for (key, value) in metadata.0.into_iter() {
            // Write name
            Self::write_name(writer, key);

            // Save position for metadata type in string

            // Match the data type
            let data_type_string = match value {
                MetadataValue::String(_) => String::from("string"),
                MetadataValue::Bool(_) => String::from("bool"),
                MetadataValue::I32(_) => String::from("int32"),
                MetadataValue::I64(_) => String::from("int64"),
                MetadataValue::Float(_) => String::from("float"),
                MetadataValue::Vec3i(_) => String::from("vec3i"),
                MetadataValue::Unknown { name, data } => name,
            };

            Self::write_name(writer, data_type_string);

            // Match the data type
            let data_len = match value {
                MetadataValue::String(s) => s.len(),
                MetadataValue::Unknown { name, data } => data.len(),
                _ => 0, // This could be anything ??
            };

            // Write len of data
            writer.write(&data_len.to_le_bytes());

            // Write each data
            match value {
                MetadataValue::String(s) => Self::write_string(writer, s),
                MetadataValue::Bool(b) => writer.write_all(&[if b { 1u8 } else { 0u8 }]).is_ok(),
                MetadataValue::I32(i32) => writer.write(&i32.to_le_bytes()).is_ok(),
                MetadataValue::I64(i64) => writer.write(&i64.to_le_bytes()).is_ok(),
                MetadataValue::Float(f) => writer.write(&f.to_le_bytes()).is_ok(),
                MetadataValue::Vec3i(iv) => Self::write_i_vec3(writer, iv),
                MetadataValue::Unknown { name, data } => writer.write(&data).is_ok(),
            };
        }
    }

    fn write_name(writer: &mut W, string: String) -> bool {
        writer.write(&string.len().to_le_bytes());
        Self::write_string(writer, string)
    }

    fn write_string(writer: &mut W, string: String) -> bool {
        for i in 0..string.len() {
            if writer
                .write_all(&[string.chars().nth(i).unwrap() as u8])
                .is_err()
            {
                return false;
            }
        }
        true
    }

    fn write_i_vec3(writer: &mut W, iv: IVec3) -> bool {
        writer.write(&iv.x.to_le_bytes()).is_err()
            || writer.write(&iv.y.to_le_bytes()).is_err()
            || writer.write(&iv.z.to_le_bytes()).is_err()
    }
}
