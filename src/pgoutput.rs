//! Decoder for the subset of PostgreSQL's pgoutput protocol used by the count
//! worker. It reads protocol version 1 binary messages returned by a logical
//! replication slot, remembers the source relation's column positions, and
//! converts row messages into `RowChange` values. Transaction boundaries and
//! commit LSNs stay visible to the worker so it can apply or skip a whole
//! transaction. The module also converts SPI's hex-encoded message bytes.

use crate::engine::RowChange;

#[derive(Default)]
pub struct Decoder {
    relation_id: Option<u32>,
    id_column: usize,
    group_column: usize,
}

pub enum Message {
    Begin,
    Change(RowChange),
    Commit(u64),
    Truncate,
    Other,
}

#[derive(Clone)]
enum Column {
    Text(String),
    Null,
    Unchanged,
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
    fn take(&mut self, len: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or("pgoutput length overflow")?;
        let value = self
            .bytes
            .get(self.pos..end)
            .ok_or("truncated pgoutput message")?;
        self.pos = end;
        Ok(value)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn cstr(&mut self) -> Result<String, String> {
        let rest = &self.bytes[self.pos..];
        let len = rest
            .iter()
            .position(|b| *b == 0)
            .ok_or("unterminated pgoutput string")?;
        let value = std::str::from_utf8(self.take(len + 1)?[..len].as_ref())
            .map_err(|_| "non-UTF8 pgoutput string")?;
        Ok(value.to_owned())
    }
    fn tuple(&mut self) -> Result<Vec<Column>, String> {
        let count = self.u16()? as usize;
        let mut columns = Vec::with_capacity(count);
        for _ in 0..count {
            columns.push(match self.u8()? {
                b'n' => Column::Null,
                b'u' => Column::Unchanged,
                b't' => {
                    let len = self.u32()? as usize;
                    Column::Text(
                        std::str::from_utf8(self.take(len)?)
                            .map_err(|_| "non-UTF8 tuple value")?
                            .to_owned(),
                    )
                }
                _ => return Err("unsupported pgoutput tuple field".into()),
            });
        }
        Ok(columns)
    }
}

fn text(columns: &[Column], index: usize) -> Result<Option<String>, String> {
    match columns.get(index).ok_or("missing tuple column")? {
        Column::Text(value) => Ok(Some(value.clone())),
        Column::Unchanged => Ok(None),
        Column::Null => Err("NULL in required source column".into()),
    }
}

fn id(columns: &[Column], index: usize) -> Result<i64, String> {
    text(columns, index)?
        .ok_or_else(|| "unchanged source id".to_owned())?
        .parse()
        .map_err(|_| "invalid bigint source id".into())
}

fn key_id(columns: &[Column], index: usize) -> Result<i64, String> {
    id(columns, if columns.len() == 1 { 0 } else { index })
}

impl Decoder {
    /// Decode one message; relation metadata determines where `id` and
    /// `group_key` appear, so source column order need not be fixed.
    pub fn parse(&mut self, bytes: &[u8], source_oid: u32) -> Result<Message, String> {
        let mut reader = Reader::new(bytes);
        let tag = reader.u8()?;
        match tag {
            b'B' => Ok(Message::Begin),
            b'C' => {
                reader.u8()?; // flags
                reader.u64()?; // commit LSN
                Ok(Message::Commit(reader.u64()?))
            }
            b'R' => {
                let oid = reader.u32()?;
                reader.cstr()?; // namespace
                reader.cstr()?; // table name
                reader.u8()?; // replica identity
                let count = reader.u16()?;
                if oid == source_oid {
                    let mut id_column = None;
                    let mut group_column = None;
                    for i in 0..count as usize {
                        reader.u8()?; // key flag
                        let name = reader.cstr()?;
                        let type_oid = reader.u32()?;
                        reader.u32()?; // type modifier
                        if name == "id" && type_oid == 20 {
                            id_column = Some(i);
                        }
                        if name == "group_key" && type_oid == 25 {
                            group_column = Some(i);
                        }
                    }
                    self.relation_id = Some(oid);
                    self.id_column = id_column.ok_or("source id column changed")?;
                    self.group_column = group_column.ok_or("source group_key column changed")?;
                }
                Ok(Message::Other)
            }
            b'I' | b'U' | b'D' => {
                let oid = reader.u32()?;
                if oid != source_oid {
                    return Ok(Message::Other);
                }
                if self.relation_id != Some(oid) {
                    return Err("missing source relation metadata".into());
                }
                let first = reader.u8()?;
                match tag {
                    b'I' => {
                        if first != b'N' {
                            return Err("invalid insert tuple".into());
                        }
                        let tuple = reader.tuple()?;
                        Ok(Message::Change(RowChange::Insert {
                            id: id(&tuple, self.id_column)?,
                            group: text(&tuple, self.group_column)?
                                .ok_or("unchanged inserted group")?,
                        }))
                    }
                    b'U' => {
                        let (old, marker) = if first == b'K' || first == b'O' {
                            let old = reader.tuple()?;
                            (Some(old), reader.u8()?)
                        } else {
                            (None, first)
                        };
                        if marker != b'N' {
                            return Err("invalid update tuple".into());
                        }
                        let new = reader.tuple()?;
                        let new_id = id(&new, self.id_column)?;
                        let old_id = match old {
                            Some(old) => key_id(&old, self.id_column)?,
                            None => new_id,
                        };
                        Ok(Message::Change(RowChange::Update {
                            old_id,
                            id: new_id,
                            group: text(&new, self.group_column)?,
                        }))
                    }
                    b'D' => {
                        if first != b'K' && first != b'O' {
                            return Err("invalid delete tuple".into());
                        }
                        let old = reader.tuple()?;
                        Ok(Message::Change(RowChange::Delete {
                            id: key_id(&old, self.id_column)?,
                        }))
                    }
                    _ => unreachable!(),
                }
            }
            b'T' => Ok(Message::Truncate),
            _ => Ok(Message::Other),
        }
    }
}

pub fn from_hex(input: &str) -> Result<Vec<u8>, String> {
    fn digit(c: u8) -> Result<u8, String> {
        (c as char)
            .to_digit(16)
            .map(|n| n as u8)
            .ok_or("invalid hex digit".into())
    }
    let bytes = input.as_bytes();
    if bytes.len() % 2 != 0 {
        return Err("odd-length hex".into());
    }
    bytes
        .chunks_exact(2)
        .map(|pair| Ok(digit(pair[0])? << 4 | digit(pair[1])?))
        .collect()
}

pub fn lsn_string(lsn: u64) -> String {
    format!("{:X}/{:08X}", lsn >> 32, lsn as u32)
}
