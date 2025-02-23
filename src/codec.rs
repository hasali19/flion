use std::collections::BTreeMap;
use std::io::{self, Seek, Write};

use byteorder::{NativeEndian, ReadBytesExt, WriteBytesExt};

type ReadCursor<'a> = std::io::Cursor<&'a [u8]>;
type WriteCursor<'a> = std::io::Cursor<&'a mut Vec<u8>>;

#[repr(u8)]
enum EncodedType {
    Null = 0,
    True,
    False,
    Int32,
    Int64,
    LargeInt,
    Float64,
    String,
    UInt8List,
    Int32List,
    Int64List,
    Float64List,
    List,
    Map,
    Float32List,
}

impl From<u8> for EncodedType {
    fn from(value: u8) -> Self {
        use EncodedType::*;
        match value {
            0 => Null,
            1 => True,
            2 => False,
            3 => Int32,
            4 => Int64,
            5 => LargeInt,
            6 => Float64,
            7 => String,
            8 => UInt8List,
            9 => Int32List,
            10 => Int64List,
            11 => Float64List,
            12 => List,
            13 => Map,
            14 => Float32List,
            _ => panic!("invalid EncodedType: {value}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Float64(pub f64);

impl Eq for Float64 {}

impl PartialOrd for Float64 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.0.partial_cmp(&other.0)
    }
}

impl Ord for Float64 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.partial_cmp(other).unwrap()
    }
}

impl From<f64> for Float64 {
    fn from(value: f64) -> Self {
        Float64(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EncodableValue<'a> {
    Null,
    Bool(bool),
    I32(i32),
    I64(i64),
    F64(Float64),
    Str(&'a str),
    Map(BTreeMap<EncodableValue<'a>, EncodableValue<'a>>),
}

impl<'a> EncodableValue<'a> {
    pub fn as_bool(&self) -> Option<bool> {
        if let Self::Bool(v) = self {
            Some(*v)
        } else {
            None
        }
    }

    pub fn as_i32(&self) -> Option<&i32> {
        if let Self::I32(v) = self {
            Some(v)
        } else {
            None
        }
    }

    pub fn as_i64(&self) -> Option<&i64> {
        if let Self::I64(v) = self {
            Some(v)
        } else {
            None
        }
    }

    pub fn as_string(&self) -> Option<&'a str> {
        if let Self::Str(v) = self {
            Some(v)
        } else {
            None
        }
    }

    pub fn as_map(&self) -> Option<&BTreeMap<EncodableValue<'a>, EncodableValue<'a>>> {
        if let Self::Map(v) = self {
            Some(v)
        } else {
            None
        }
    }
}

fn read_size(cursor: &mut ReadCursor) -> io::Result<u32> {
    match cursor.read_u8()? {
        254 => cursor.read_u16::<NativeEndian>().map(u32::from),
        255 => cursor.read_u32::<NativeEndian>(),
        v => Ok(v.into()),
    }
}

fn write_size(w: &mut impl io::Write, value: u32) -> io::Result<()> {
    if value < 254 {
        w.write_u8(value as u8)?;
    } else if value <= u16::MAX.into() {
        w.write_u8(254)?;
        w.write_u16::<NativeEndian>(value as u16)?;
    } else {
        w.write_u8(255)?;
        w.write_u32::<NativeEndian>(value)?;
    }
    Ok(())
}

fn read_string<'a>(cursor: &mut ReadCursor<'a>) -> io::Result<&'a str> {
    let size = read_size(cursor)?;
    let buf = &cursor.get_ref()[cursor.position() as usize..][..size as usize];
    cursor.set_position(cursor.position() + size as u64);
    std::str::from_utf8(buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn write_string(w: &mut WriteCursor, value: &str) -> io::Result<()> {
    write_size(w, value.len() as u32)?;
    w.write_all(value.as_bytes())?;
    Ok(())
}

fn read_map<'a>(
    cursor: &mut ReadCursor<'a>,
) -> io::Result<BTreeMap<EncodableValue<'a>, EncodableValue<'a>>> {
    let size = read_size(cursor)?;
    let mut map = BTreeMap::new();
    for _ in 0..size {
        let key = read_value(cursor)?;
        let value = read_value(cursor)?;
        map.insert(key, value);
    }
    Ok(map)
}

fn write_map(
    w: &mut WriteCursor,
    value: &BTreeMap<EncodableValue, EncodableValue>,
) -> io::Result<()> {
    write_size(w, value.len() as u32)?;
    for (k, v) in value {
        write_value(w, k)?;
        write_value(w, v)?;
    }
    Ok(())
}

pub fn read_value<'a>(cursor: &mut ReadCursor<'a>) -> io::Result<EncodableValue<'a>> {
    let encoded_type = EncodedType::from(cursor.read_u8()?);
    match encoded_type {
        EncodedType::Null => Ok(EncodableValue::Null),
        EncodedType::True => Ok(EncodableValue::Bool(true)),
        EncodedType::False => Ok(EncodableValue::Bool(false)),
        EncodedType::Int32 => Ok(EncodableValue::I32(cursor.read_i32::<NativeEndian>()?)),
        EncodedType::Int64 => Ok(EncodableValue::I64(cursor.read_i64::<NativeEndian>()?)),
        EncodedType::LargeInt => todo!(),
        EncodedType::Float64 => Ok(EncodableValue::F64(Float64(
            cursor.read_f64::<NativeEndian>()?,
        ))),
        EncodedType::String => Ok(EncodableValue::Str(read_string(cursor)?)),
        EncodedType::UInt8List => todo!(),
        EncodedType::Int32List => todo!(),
        EncodedType::Int64List => todo!(),
        EncodedType::Float64List => todo!(),
        EncodedType::List => todo!(),
        EncodedType::Map => Ok(EncodableValue::Map(read_map(cursor)?)),
        EncodedType::Float32List => todo!(),
    }
}

pub fn write_value(w: &mut WriteCursor, value: &EncodableValue) -> io::Result<()> {
    match value {
        EncodableValue::Null => {
            w.write_u8(EncodedType::Null as u8)?;
        }
        EncodableValue::Bool(v) => {
            if *v {
                w.write_u8(EncodedType::True as u8)?;
            } else {
                w.write_u8(EncodedType::False as u8)?;
            }
        }
        EncodableValue::I32(v) => {
            w.write_u8(EncodedType::Int32 as u8)?;
            w.write_i32::<NativeEndian>(*v)?;
        }
        EncodableValue::I64(v) => {
            w.write_u8(EncodedType::Int64 as u8)?;
            w.write_i64::<NativeEndian>(*v)?;
        }
        EncodableValue::F64(v) => {
            w.write_u8(EncodedType::Float64 as u8)?;
            align_to(w, 8)?;
            w.write_f64::<NativeEndian>(v.0)?;
        }
        EncodableValue::Str(v) => {
            w.write_u8(EncodedType::String as u8)?;
            write_string(w, v)?;
        }
        EncodableValue::Map(v) => {
            w.write_u8(EncodedType::Map as u8)?;
            write_map(w, v)?;
        }
    }

    Ok(())
}

fn align_to(w: &mut WriteCursor, align: usize) -> io::Result<()> {
    let m = w.stream_position()? as usize % align;
    if m == 0 {
        return Ok(());
    }
    let m = align - m;
    for _ in 0..m {
        w.write_u8(0)?;
    }
    Ok(())
}
