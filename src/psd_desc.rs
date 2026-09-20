//! Photoshop's descriptor structure, the typed tree the layer effects (`lfx2`) and type tool settings
//! (`TySh`) blocks are written in: read into a small value tree and written back from one.

use anyhow::{Result, bail};

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Desc(Descriptor),
    List(Vec<Item>),
    Double(f64),
    /// A unit float: the unit ("#Prc", "#Pxl", "#Ang") and the value.
    Unit(String, f64),
    Text(String),
    /// An enumeration: its type and value ("BlnM", "Mltp").
    Enum(String, String),
    Int(i32),
    Bool(bool),
    Class(String),
    Data(Vec<u8>),
    Other,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Descriptor { pub name: String, pub class: String, pub items: Vec<(String, Item)> }

impl Descriptor {
    pub fn new(class: &str) -> Descriptor { Descriptor { name: String::new(), class: class.into(), items: Vec::new() } }
    pub fn get(&self, key: &str) -> Option<&Item> { self.items.iter().find(|(k, _)| k == key).map(|(_, v)| v) }
    pub fn desc(&self, key: &str) -> Option<&Descriptor> { match self.get(key) { Some(Item::Desc(d)) => Some(d), _ => None } }
    pub fn number(&self, key: &str) -> Option<f64> { match self.get(key) { Some(Item::Double(v)) | Some(Item::Unit(_, v)) => Some(*v), Some(Item::Int(i)) => Some(*i as f64), _ => None } }
    pub fn boolean(&self, key: &str) -> Option<bool> { match self.get(key) { Some(Item::Bool(b)) => Some(*b), _ => None } }
    pub fn text(&self, key: &str) -> Option<&str> { match self.get(key) { Some(Item::Text(t)) => Some(t), _ => None } }
    pub fn enum_value(&self, key: &str) -> Option<&str> { match self.get(key) { Some(Item::Enum(_, v)) => Some(v), _ => None } }
    pub fn data(&self, key: &str) -> Option<&[u8]> { match self.get(key) { Some(Item::Data(d)) => Some(d), _ => None } }
    /// A color object ("RGBC" with Rd, Grn, Bl from 0 to 255) as 0 to 1.
    pub fn color(&self, key: &str) -> Option<[f64; 3]> { let c = self.desc(key)?; Some([c.number("Rd  ")? / 255.0, c.number("Grn ")? / 255.0, c.number("Bl  ")? / 255.0]) }
    pub fn push(&mut self, key: &str, item: Item) -> &mut Self { self.items.push((key.into(), item)); self }
}

pub fn rgb(color: [f64; 3]) -> Item {
    let mut c = Descriptor::new("RGBC");
    c.push("Rd  ", Item::Double(color[0] * 255.0)).push("Grn ", Item::Double(color[1] * 255.0)).push("Bl  ", Item::Double(color[2] * 255.0));
    Item::Desc(c)
}

struct R<'a> { d: &'a [u8], p: usize }
impl<'a> R<'a> {
    fn need(&self, n: usize) -> Result<()> { if self.p + n > self.d.len() { bail!("the descriptor ends early"); } Ok(()) }
    fn u8(&mut self) -> Result<u8> { self.need(1)?; let v = self.d[self.p]; self.p += 1; Ok(v) }
    fn u32(&mut self) -> Result<u32> { self.need(4)?; let v = u32::from_be_bytes(self.d[self.p..self.p + 4].try_into().unwrap()); self.p += 4; Ok(v) }
    fn f64(&mut self) -> Result<f64> { self.need(8)?; let v = f64::from_be_bytes(self.d[self.p..self.p + 8].try_into().unwrap()); self.p += 8; Ok(v) }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> { self.need(n)?; let s = &self.d[self.p..self.p + n]; self.p += n; Ok(s) }
    fn unicode(&mut self) -> Result<String> {
        let n = self.u32()? as usize;
        if n > 1 << 20 { bail!("a descriptor string is too long"); }
        let raw = self.bytes(n * 2)?;
        let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        Ok(String::from_utf16_lossy(&units).trim_end_matches('\0').to_string())
    }
    /// A key or class id: four characters when the length is 0, else that many bytes.
    fn key(&mut self) -> Result<String> {
        let n = self.u32()? as usize;
        let raw = if n == 0 { self.bytes(4)? } else { if n > 4096 { bail!("a descriptor key is too long"); } self.bytes(n)? };
        Ok(String::from_utf8_lossy(raw).to_string())
    }
}

/// Reads one descriptor (the structure after its version number).
pub fn parse(data: &[u8]) -> Result<(Descriptor, usize)> {
    let mut r = R { d: data, p: 0 };
    let d = descriptor(&mut r, 0)?;
    Ok((d, r.p))
}

fn descriptor(r: &mut R, depth: usize) -> Result<Descriptor> {
    if depth > 32 { bail!("descriptors nest too deeply"); }
    let name = r.unicode()?;
    let class = r.key()?;
    let count = r.u32()? as usize;
    if count > 100_000 { bail!("a descriptor claims {count} items"); }
    let mut items = Vec::with_capacity(count.min(256));
    for _ in 0..count {
        let key = r.key()?;
        let value = item(r, depth)?;
        items.push((key, value));
    }
    Ok(Descriptor { name, class, items })
}

fn item(r: &mut R, depth: usize) -> Result<Item> {
    let kind = r.bytes(4)?;
    Ok(match kind {
        b"Objc" | b"GlbO" => Item::Desc(descriptor(r, depth + 1)?),
        b"VlLs" => {
            let n = r.u32()? as usize;
            if n > 100_000 { bail!("a list claims {n} items"); }
            let mut list = Vec::with_capacity(n.min(256));
            for _ in 0..n { list.push(item(r, depth + 1)?); }
            Item::List(list)
        }
        b"doub" => Item::Double(r.f64()?),
        b"UntF" => { let unit = String::from_utf8_lossy(r.bytes(4)?).to_string(); Item::Unit(unit, r.f64()?) }
        b"TEXT" => Item::Text(r.unicode()?),
        b"enum" => { let t = r.key()?; let v = r.key()?; Item::Enum(t, v) }
        b"long" => Item::Int(r.u32()? as i32),
        b"comp" => { r.bytes(8)?; Item::Other }
        b"bool" => Item::Bool(r.u8()? != 0),
        b"type" | b"GlbC" => { let _ = r.unicode()?; Item::Class(r.key()?) }
        b"alis" | b"tdta" => { let n = r.u32()? as usize; if n > 64 << 20 { bail!("a data item is too large"); } Item::Data(r.bytes(n)?.to_vec()) }
        b"obj " => {
            // A reference: parsed for its length, kept as nothing.
            let n = r.u32()? as usize;
            for _ in 0..n.min(10_000) {
                let t = r.bytes(4)?;
                match t {
                    b"prop" => { r.unicode()?; r.key()?; r.key()?; }
                    b"Clss" => { r.unicode()?; r.key()?; }
                    b"Enmr" => { r.unicode()?; r.key()?; r.key()?; r.key()?; }
                    b"rele" => { r.unicode()?; r.key()?; r.u32()?; }
                    b"Idnt" | b"indx" => { r.u32()?; }
                    b"name" => { r.unicode()?; r.key()?; r.unicode()?; }
                    other => bail!("unknown reference item {}", String::from_utf8_lossy(other)),
                }
            }
            Item::Other
        }
        other => bail!("unknown descriptor item type {}", String::from_utf8_lossy(other)),
    })
}

struct W { out: Vec<u8> }
impl W {
    fn u32(&mut self, v: u32) { self.out.extend_from_slice(&v.to_be_bytes()); }
    fn f64(&mut self, v: f64) { self.out.extend_from_slice(&v.to_be_bytes()); }
    fn unicode(&mut self, s: &str) { let units: Vec<u16> = s.encode_utf16().chain([0]).collect(); self.u32(units.len() as u32); for u in units { self.out.extend_from_slice(&u.to_be_bytes()); } }
    fn key(&mut self, k: &str) { let b = k.as_bytes(); if b.len() == 4 { self.u32(0); self.out.extend_from_slice(b); } else { self.u32(b.len() as u32); self.out.extend_from_slice(b); } }
}

/// Writes a descriptor (without a version number in front).
pub fn write(d: &Descriptor) -> Vec<u8> {
    let mut w = W { out: Vec::new() };
    write_descriptor(&mut w, d);
    w.out
}

fn write_descriptor(w: &mut W, d: &Descriptor) {
    w.unicode(&d.name);
    w.key(&d.class);
    w.u32(d.items.len() as u32);
    for (k, v) in &d.items { w.key(k); write_item(w, v); }
}

fn write_item(w: &mut W, v: &Item) {
    match v {
        Item::Desc(d) => { w.out.extend_from_slice(b"Objc"); write_descriptor(w, d); }
        Item::List(l) => { w.out.extend_from_slice(b"VlLs"); w.u32(l.len() as u32); for i in l { write_item(w, i); } }
        Item::Double(x) => { w.out.extend_from_slice(b"doub"); w.f64(*x); }
        Item::Unit(u, x) => { w.out.extend_from_slice(b"UntF"); let mut b = u.as_bytes().to_vec(); b.resize(4, b' '); w.out.extend_from_slice(&b); w.f64(*x); }
        Item::Text(t) => { w.out.extend_from_slice(b"TEXT"); w.unicode(t); }
        Item::Enum(t, e) => { w.out.extend_from_slice(b"enum"); w.key(t); w.key(e); }
        Item::Int(i) => { w.out.extend_from_slice(b"long"); w.u32(*i as u32); }
        Item::Bool(b) => { w.out.extend_from_slice(b"bool"); w.out.push(*b as u8); }
        Item::Class(c) => { w.out.extend_from_slice(b"type"); w.unicode(""); w.key(c); }
        Item::Data(d) => { w.out.extend_from_slice(b"tdta"); w.u32(d.len() as u32); w.out.extend_from_slice(d); }
        Item::Other => { w.out.extend_from_slice(b"bool"); w.out.push(0); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_round_trip() {
        let mut d = Descriptor::new("null");
        d.push("Scl ", Item::Unit("#Prc".into(), 100.0)).push("masterFXSwitch", Item::Bool(true)).push("Txt ", Item::Text("Hello, wörld".into()));
        let mut inner = Descriptor::new("DrSh");
        inner.push("enab", Item::Bool(true)).push("Md  ", Item::Enum("BlnM".into(), "Mltp".into())).push("Clr ", rgb([1.0, 0.5, 0.0])).push("lagl", Item::Unit("#Ang".into(), 120.0)).push("n", Item::Int(-3)).push("list", Item::List(vec![Item::Double(1.5), Item::Text("x".into())])).push("raw", Item::Data(vec![1, 2, 3]));
        d.push("DrSh", Item::Desc(inner));
        let bytes = write(&d);
        let (back, used) = parse(&bytes).unwrap();
        assert_eq!(used, bytes.len());
        assert_eq!(back, d);
        assert_eq!(back.desc("DrSh").unwrap().color("Clr "), Some([1.0, 0.5, 0.0]));
        assert_eq!(back.desc("DrSh").unwrap().enum_value("Md  "), Some("Mltp"));
        assert_eq!(back.number("Scl "), Some(100.0));
        assert!(parse(&bytes[..20]).is_err());
    }
}
