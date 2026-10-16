//! The messages whose maps keep their order: `Document`, `MapValue` and `AggregationResult`, and
//! Datastore mode's `Entity` (#81).
//!
//! prost generates a protobuf map as a `HashMap` or a `BTreeMap`. The official emulator returns
//! a document's fields in the order it holds them, which depends on how they were written
//! (docs/parity-exceptions.md, #108), so these three messages are written by hand around
//! [`Fields`], a map that keeps insertion order, and take the generated messages' place through
//! `extern_path` (build.rs). On the wire they are the same messages, with entries in order.

use std::{collections::BTreeMap, fmt, ops::Index};

use indexmap::IndexMap;
use prost::{
    DecodeError, Message,
    bytes::{Buf, BufMut},
    encoding::{self, DecodeContext, WireType},
};

use crate::google::{datastore::v1 as datastore, firestore::v1::Value};

/// Field names and values in insertion order. Inserting a name that is already there keeps its
/// place; removing one keeps the order of the others. Equality ignores the order, as for
/// protobuf maps.
#[derive(Clone, PartialEq)]
pub struct Ordered<V>(IndexMap<String, V>);

/// A Firestore document's or map's fields.
pub type Fields = Ordered<Value>;

/// A Datastore entity's properties (#81).
pub type Properties = Ordered<datastore::Value>;

impl<V> Default for Ordered<V> {
    fn default() -> Self {
        Self(IndexMap::new())
    }
}

impl<V> Ordered<V> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self(IndexMap::with_capacity(capacity))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&V> {
        self.0.get(key)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        self.0.get_mut(key)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    /// Sets `key`, in place if it is there, else at the end.
    pub fn insert(&mut self, key: String, value: V) -> Option<V> {
        self.0.insert(key, value)
    }

    /// Removes `key`; the fields after it keep their order.
    pub fn remove(&mut self, key: &str) -> Option<V> {
        self.0.shift_remove(key)
    }

    /// Sets `key` at the end, moving it there if it is already present.
    pub fn push(&mut self, key: String, value: V) {
        self.0.shift_remove(&key);
        self.0.insert(key, value);
    }

    pub fn entry(&mut self, key: String) -> indexmap::map::Entry<'_, String, V> {
        self.0.entry(key)
    }

    pub fn iter(&self) -> indexmap::map::Iter<'_, String, V> {
        self.0.iter()
    }

    pub fn iter_mut(&mut self) -> indexmap::map::IterMut<'_, String, V> {
        self.0.iter_mut()
    }

    pub fn keys(&self) -> indexmap::map::Keys<'_, String, V> {
        self.0.keys()
    }

    pub fn values(&self) -> indexmap::map::Values<'_, String, V> {
        self.0.values()
    }

    pub fn values_mut(&mut self) -> indexmap::map::ValuesMut<'_, String, V> {
        self.0.values_mut()
    }

    /// The fields in Firestore's canonical order: names by UTF-8 bytes, which is `String`'s
    /// `Ord`. Comparisons and index keys use this order, never the stored one.
    pub fn sorted(&self) -> Vec<(&String, &V)> {
        let mut entries: Vec<_> = self.0.iter().collect();
        entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
        entries
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }
}

impl<V: fmt::Debug> fmt::Debug for Ordered<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.0.iter()).finish()
    }
}

impl<V> FromIterator<(String, V)> for Ordered<V> {
    fn from_iter<I: IntoIterator<Item = (String, V)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl<V, const N: usize> From<[(String, V); N]> for Ordered<V> {
    fn from(entries: [(String, V); N]) -> Self {
        entries.into_iter().collect()
    }
}

/// Panics when `key` is missing, as `BTreeMap` does.
impl<V> Index<&str> for Ordered<V> {
    type Output = V;

    fn index(&self, key: &str) -> &V {
        &self.0[key]
    }
}

impl<V> Extend<(String, V)> for Ordered<V> {
    fn extend<I: IntoIterator<Item = (String, V)>>(&mut self, iter: I) {
        self.0.extend(iter);
    }
}

impl<V> IntoIterator for Ordered<V> {
    type Item = (String, V);
    type IntoIter = indexmap::map::IntoIter<String, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a, V> IntoIterator for &'a Ordered<V> {
    type Item = (&'a String, &'a V);
    type IntoIter = indexmap::map::Iter<'a, String, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a, V> IntoIterator for &'a mut Ordered<V> {
    type Item = (&'a String, &'a mut V);
    type IntoIter = indexmap::map::IterMut<'a, String, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}

/// A Firestore document. Same fields and tags as `google.firestore.v1.Document`.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Document {
    pub name: String,
    pub fields: Fields,
    pub create_time: Option<prost_types::Timestamp>,
    pub update_time: Option<prost_types::Timestamp>,
}

/// A map value. Same fields and tags as `google.firestore.v1.MapValue`.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct MapValue {
    pub fields: Fields,
}

/// The result of one aggregation. Same fields and tags as
/// `google.firestore.v1.AggregationResult`.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct AggregationResult {
    pub aggregate_fields: Fields,
}

/// One entry per field, in order, each encoded as prost encodes a map entry: the key and the
/// value are left out when they are empty.
fn encode_fields<V: Message + Default + PartialEq>(
    tag: u32,
    fields: &Ordered<V>,
    buf: &mut impl BufMut,
) {
    for (key, value) in fields {
        encoding::encode_key(tag, WireType::LengthDelimited, buf);
        encoding::encode_varint(entry_len(key, value) as u64, buf);
        if !key.is_empty() {
            encoding::string::encode(1, key, buf);
        }
        if *value != V::default() {
            encoding::message::encode(2, value, buf);
        }
    }
}

fn entry_len<V: Message + Default + PartialEq>(key: &String, value: &V) -> usize {
    let key = if key.is_empty() {
        0
    } else {
        encoding::string::encoded_len(1, key)
    };
    let value = if *value == V::default() {
        0
    } else {
        encoding::message::encoded_len(2, value)
    };
    key + value
}

fn fields_len<V: Message + Default + PartialEq>(tag: u32, fields: &Ordered<V>) -> usize {
    fields
        .iter()
        .map(|(key, value)| {
            let len = entry_len(key, value);
            encoding::key_len(tag) + encoding::encoded_len_varint(len as u64) + len
        })
        .sum()
}

/// One occurrence of a map field is one entry. prost decodes it (framing, unknown fields,
/// recursion limit); a name seen again keeps its first place and takes the new value, as
/// protobuf's "last one wins" and Java's `LinkedHashMap` both have it.
fn merge_fields<V: Message + Default>(
    wire_type: WireType,
    fields: &mut Ordered<V>,
    buf: &mut impl Buf,
    ctx: DecodeContext,
) -> Result<(), DecodeError> {
    encoding::check_wire_type(WireType::LengthDelimited, wire_type)?;
    let mut entry = BTreeMap::new();
    encoding::btree_map::merge(
        encoding::string::merge,
        encoding::message::merge,
        &mut entry,
        buf,
        ctx,
    )?;
    fields.extend(entry);
    Ok(())
}

fn context(message: &'static str, field: &'static str) -> impl Fn(DecodeError) -> DecodeError {
    move |mut error| {
        error.push(message, field);
        error
    }
}

impl Message for Document {
    fn encode_raw(&self, buf: &mut impl BufMut) {
        if !self.name.is_empty() {
            encoding::string::encode(1, &self.name, buf);
        }
        encode_fields(2, &self.fields, buf);
        if let Some(time) = &self.create_time {
            encoding::message::encode(3, time, buf);
        }
        if let Some(time) = &self.update_time {
            encoding::message::encode(4, time, buf);
        }
    }

    fn merge_field(
        &mut self,
        tag: u32,
        wire_type: WireType,
        buf: &mut impl Buf,
        ctx: DecodeContext,
    ) -> Result<(), DecodeError> {
        match tag {
            1 => encoding::string::merge(wire_type, &mut self.name, buf, ctx)
                .map_err(context("Document", "name")),
            2 => merge_fields(wire_type, &mut self.fields, buf, ctx)
                .map_err(context("Document", "fields")),
            3 => encoding::message::merge(
                wire_type,
                self.create_time.get_or_insert_with(Default::default),
                buf,
                ctx,
            )
            .map_err(context("Document", "create_time")),
            4 => encoding::message::merge(
                wire_type,
                self.update_time.get_or_insert_with(Default::default),
                buf,
                ctx,
            )
            .map_err(context("Document", "update_time")),
            _ => encoding::skip_field(wire_type, tag, buf, ctx),
        }
    }

    fn encoded_len(&self) -> usize {
        let name = if self.name.is_empty() {
            0
        } else {
            encoding::string::encoded_len(1, &self.name)
        };
        name + fields_len(2, &self.fields)
            + self
                .create_time
                .as_ref()
                .map_or(0, |time| encoding::message::encoded_len(3, time))
            + self
                .update_time
                .as_ref()
                .map_or(0, |time| encoding::message::encoded_len(4, time))
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

impl Message for MapValue {
    fn encode_raw(&self, buf: &mut impl BufMut) {
        encode_fields(1, &self.fields, buf);
    }

    fn merge_field(
        &mut self,
        tag: u32,
        wire_type: WireType,
        buf: &mut impl Buf,
        ctx: DecodeContext,
    ) -> Result<(), DecodeError> {
        match tag {
            1 => merge_fields(wire_type, &mut self.fields, buf, ctx)
                .map_err(context("MapValue", "fields")),
            _ => encoding::skip_field(wire_type, tag, buf, ctx),
        }
    }

    fn encoded_len(&self) -> usize {
        fields_len(1, &self.fields)
    }

    fn clear(&mut self) {
        self.fields.clear();
    }
}

impl Message for AggregationResult {
    fn encode_raw(&self, buf: &mut impl BufMut) {
        encode_fields(2, &self.aggregate_fields, buf);
    }

    fn merge_field(
        &mut self,
        tag: u32,
        wire_type: WireType,
        buf: &mut impl Buf,
        ctx: DecodeContext,
    ) -> Result<(), DecodeError> {
        match tag {
            2 => merge_fields(wire_type, &mut self.aggregate_fields, buf, ctx)
                .map_err(context("AggregationResult", "aggregate_fields")),
            _ => encoding::skip_field(wire_type, tag, buf, ctx),
        }
    }

    fn encoded_len(&self) -> usize {
        fields_len(2, &self.aggregate_fields)
    }

    fn clear(&mut self) {
        self.aggregate_fields.clear();
    }
}

/// A Datastore entity. Same fields and tags as `google.datastore.v1.Entity`: its properties
/// keep the order they were written in, as the official emulator returns them (#81).
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Entity {
    pub key: Option<datastore::Key>,
    pub properties: Properties,
}

impl Message for Entity {
    fn encode_raw(&self, buf: &mut impl BufMut) {
        if let Some(key) = &self.key {
            encoding::message::encode(1, key, buf);
        }
        encode_fields(3, &self.properties, buf);
    }

    fn merge_field(
        &mut self,
        tag: u32,
        wire_type: WireType,
        buf: &mut impl Buf,
        ctx: DecodeContext,
    ) -> Result<(), DecodeError> {
        match tag {
            1 => encoding::message::merge(
                wire_type,
                self.key.get_or_insert_with(Default::default),
                buf,
                ctx,
            )
            .map_err(context("Entity", "key")),
            3 => merge_fields(wire_type, &mut self.properties, buf, ctx)
                .map_err(context("Entity", "properties")),
            _ => encoding::skip_field(wire_type, tag, buf, ctx),
        }
    }

    fn encoded_len(&self) -> usize {
        self.key
            .as_ref()
            .map_or(0, |key| encoding::message::encoded_len(1, key))
            + fields_len(3, &self.properties)
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}
