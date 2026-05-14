use rkyv::{Archive, Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Archive, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(compare(PartialEq))]
#[rkyv(derive(Debug))]
pub struct StrRef {
    pub offset: u32,
    pub len: u32,
}

#[derive(Default)]
pub struct StringPool {
    pub bytes: Vec<u8>,
    pub index: HashMap<String, u32>,
}

impl StringPool {
    pub fn new() -> Self {
        Self {
            bytes: Vec::new(),
            index: HashMap::new(),
        }
    }

    pub fn add(&mut self, s: &str) -> StrRef {
        if let Some(&offset) = self.index.get(s) {
            return StrRef {
                offset,
                len: s.len() as u32,
            };
        }

        let offset = self.bytes.len() as u32;
        let len = s.len() as u32;
        self.bytes.extend_from_slice(s.as_bytes());
        self.index.insert(s.to_string(), offset);

        StrRef { offset, len }
    }

    pub fn resolve(&self, str_ref: &StrRef) -> &str {
        let start = str_ref.offset as usize;
        let end = start + str_ref.len as usize;
        std::str::from_utf8(&self.bytes[start..end]).expect("Invalid UTF-8 in pool")
    }
}

// Helper for the archived version
impl ArchivedStrRef {
    pub fn resolve<'a>(&self, pool_bytes: &'a [u8]) -> &'a str {
        // rkyv 0.8.x syntax: Use `.to_native()` to handle endianness safely
        let start = self.offset.to_native() as usize;
        let end = start + self.len.to_native() as usize;
        std::str::from_utf8(&pool_bytes[start..end]).expect("Invalid UTF-8 in pool")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_string_pool_add_and_resolve() {
        let mut pool = StringPool::new();
        let ref1 = pool.add("hello");
        let ref2 = pool.add("world");
        let ref3 = pool.add("hello"); // Deduplication

        assert_eq!(pool.resolve(&ref1), "hello");
        assert_eq!(pool.resolve(&ref2), "world");
        assert_eq!(pool.resolve(&ref3), "hello");

        // ref1 and ref3 should point to the exact same offset
        assert_eq!(ref1.offset, ref3.offset);
    }
}
