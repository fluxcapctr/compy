//! Generative Fill and Generative Expand through fal.ai: the selection's surroundings go out as an image
//! and a mask (inpainting), the model paints the masked area, and the result comes back as a masked layer.
//! Only a context window around the selection leaves the machine, scaled to what the models want.

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The longest side sent to a model; the result is scaled back over the same window.
pub const MAX_SIDE: usize = 1536;
/// How much of the selection's size is added around it as context, each way.
pub const CONTEXT: f64 = 0.5;
pub const MIN_CONTEXT: f64 = 96.0;

/// A model that takes `image_url`, `mask_url` and `prompt` (white in the mask marks what to paint).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    /// Extra fields sent with every request (steps, guidance and the like).
    #[serde(default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
    /// fal's price in dollars per megapixel of output (billed rounded up), when known.
    #[serde(default)]
    pub price_per_megapixel: Option<f64>,
}

pub fn default_models() -> Vec<Model> {
    let m = |id: &str, name: &str, price: Option<f64>| Model { id: id.into(), name: name.into(), extra: Default::default(), price_per_megapixel: price };
    vec![
        m("fal-ai/flux-pro/v1/fill", "FLUX.1 Fill [pro]", Some(0.05)),
        m("fal-ai/flux-lora/inpainting", "FLUX.1 [dev] inpainting", Some(0.035)),
        m("fal-ai/qwen-image-edit/inpaint", "Qwen Image Edit inpaint", Some(0.03)),
        m("fal-ai/inpaint", "Stable Diffusion inpainting", None),
    ]
}

/// The estimated charge for `count` images of `width` x `height` pixels: fal bills each image by its
/// megapixels rounded up.
pub fn estimate(model: &Model, width: usize, height: usize, count: u32) -> Option<f64> {
    let per = model.price_per_megapixel?;
    let megapixels = ((width * height) as f64 / 1_000_000.0).ceil().max(1.0);
    Some(per * megapixels * count as f64)
}

/// The size of a context window once scaled for the model.
pub fn scaled_size(window: (i32, i32, i32, i32)) -> (usize, usize) {
    let (w, h) = ((window.2 - window.0).max(1) as f64, (window.3 - window.1).max(1) as f64);
    let scale = (MAX_SIDE as f64 / w.max(h)).min(1.0);
    (((w * scale).round() as usize).max(1), ((h * scale).round() as usize).max(1))
}

fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".config"));
    base.join("compositor")
}

/// The model list, from `~/.config/compositor/genfill-models.json`, written with the defaults when absent.
pub fn models() -> Vec<Model> {
    let path = config_dir().join("genfill-models.json");
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(mut list) = serde_json::from_str::<Vec<Model>>(&text) {
            if !list.is_empty() {
                // A list written before prices were known takes them from the defaults, by id.
                let defaults = default_models();
                for m in list.iter_mut() { if m.price_per_megapixel.is_none() { m.price_per_megapixel = defaults.iter().find(|d| d.id == m.id).and_then(|d| d.price_per_megapixel); } }
                return list;
            }
        }
    }
    let list = default_models();
    if let Some(dir) = path.parent() { let _ = std::fs::create_dir_all(dir); }
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&list).unwrap_or_default());
    list
}

/// Writes the key to `~/.config/compositor/fal.key`, readable by the user only.
pub fn save_key(key: &str) -> Result<()> {
    let key = key.trim();
    if key.is_empty() { bail!("The key is empty."); }
    let path = config_dir().join("fal.key");
    if let Some(dir) = path.parent() { std::fs::create_dir_all(dir)?; }
    std::fs::write(&path, format!("{key}\n"))?;
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)); }
    Ok(())
}

/// The fal key: `FAL_KEY`, or the first line of `~/.config/compositor/fal.key`.
pub fn key() -> Option<String> {
    if let Ok(k) = std::env::var("FAL_KEY") { let k = k.trim().to_string(); if !k.is_empty() { return Some(k); } }
    std::fs::read_to_string(config_dir().join("fal.key")).ok().and_then(|t| t.lines().next().map(|l| l.trim().to_string())).filter(|k| !k.is_empty())
}

/// What one generation asks for.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub model: Model,
    pub prompt: String,
    pub count: u32,
    pub seed: Option<u64>,
    /// PNG bytes of the context window and of its mask (white where the model paints).
    pub image_png: Vec<u8>,
    pub mask_png: Vec<u8>,
}

pub fn data_uri(png: &[u8]) -> String { format!("data:image/png;base64,{}", base64_encode(png)) }

/// The JSON body fal receives.
pub fn body(request: &Request) -> serde_json::Value {
    let mut map = request.model.extra.clone();
    map.insert("prompt".into(), serde_json::Value::String(if request.prompt.trim().is_empty() { "fill in naturally, matching the surroundings".into() } else { request.prompt.clone() }));
    map.insert("image_url".into(), serde_json::Value::String(data_uri(&request.image_png)));
    map.insert("mask_url".into(), serde_json::Value::String(data_uri(&request.mask_png)));
    map.insert("num_images".into(), serde_json::Value::from(request.count.clamp(1, 4)));
    map.insert("output_format".into(), serde_json::Value::String("png".into()));
    if let Some(seed) = request.seed { map.insert("seed".into(), serde_json::Value::from(seed)); }
    serde_json::Value::Object(map)
}

/// Where generations run: the real service, or a stand-in for tests.
pub trait Backend {
    /// Returns the PNG bytes of each image; `progress` gets a line of status now and then.
    fn generate(&self, request: &Request, progress: &dyn Fn(&str), cancelled: &dyn Fn() -> bool) -> Result<Vec<Vec<u8>>>;
}

/// fal's queue API: submit, poll, fetch, download.
pub struct Fal { pub key: String }

impl Backend for Fal {
    fn generate(&self, request: &Request, progress: &dyn Fn(&str), cancelled: &dyn Fn() -> bool) -> Result<Vec<Vec<u8>>> {
        // Some models give one image however many were asked for: ask again until the count is met.
        let wanted = request.count.clamp(1, 4) as usize;
        let mut out = Vec::new();
        let mut round = 0;
        while out.len() < wanted && round < wanted {
            round += 1;
            let mut r = request.clone();
            r.count = (wanted - out.len()) as u32;
            if round > 1 { r.seed = Some(request.seed.unwrap_or(1) + round as u64 * 7919); progress(&format!("Asking for variation {} of {}…", out.len() + 1, wanted)); }
            let mut got = self.generate_once(&r, progress, cancelled)?;
            if got.is_empty() { break; }
            out.append(&mut got);
        }
        Ok(out)
    }
}

impl Fal {
    fn generate_once(&self, request: &Request, progress: &dyn Fn(&str), cancelled: &dyn Fn() -> bool) -> Result<Vec<Vec<u8>>> {
        self.run(&request.model.id, body(request), progress, cancelled)
    }

    /// Any fal model through the queue: submit `body`, wait, fetch, and download every image in the
    /// result (`images[]`, or a single `image`).
    pub fn run(&self, model_id: &str, body: serde_json::Value, progress: &dyn Fn(&str), cancelled: &dyn Fn() -> bool) -> Result<Vec<Vec<u8>>> {
        let submit_url = format!("https://queue.fal.run/{model_id}");
        progress("Sending to fal…");
        let body = body.to_string();
        let mut response = ureq::post(&submit_url).header("Authorization", &format!("Key {}", self.key)).header("Content-Type", "application/json").send(body.as_bytes()).map_err(describe)?;
        let submitted: serde_json::Value = serde_json::from_str(&response.body_mut().read_to_string().context("reading fal's reply")?).context("fal's reply is not JSON")?;
        let status_url = submitted.get("status_url").and_then(|v| v.as_str()).context("fal gave no status URL")?.to_string();
        let response_url = submitted.get("response_url").and_then(|v| v.as_str()).context("fal gave no response URL")?.to_string();
        let cancel_url = submitted.get("cancel_url").and_then(|v| v.as_str()).map(str::to_string);
        let started = std::time::Instant::now();
        loop {
            if cancelled() {
                if let Some(url) = &cancel_url { let _ = ureq::put(url).header("Authorization", &format!("Key {}", self.key)).send_empty(); }
                bail!("Cancelled.");
            }
            std::thread::sleep(std::time::Duration::from_millis(900));
            let mut status = ureq::get(&status_url).header("Authorization", &format!("Key {}", self.key)).call().map_err(describe)?;
            let value: serde_json::Value = serde_json::from_str(&status.body_mut().read_to_string().context("reading the status")?).context("the status is not JSON")?;
            match value.get("status").and_then(|v| v.as_str()).unwrap_or("") {
                "IN_QUEUE" => progress(&format!("Waiting in fal's queue (position {})…", value.get("queue_position").and_then(|v| v.as_u64()).unwrap_or(0))),
                "IN_PROGRESS" => progress(&format!("Generating… {}s", started.elapsed().as_secs())),
                "COMPLETED" => break,
                other => bail!("fal reported {other}"),
            }
            if started.elapsed().as_secs() > 300 { bail!("fal took more than five minutes; giving up."); }
        }
        let mut result = ureq::get(&response_url).header("Authorization", &format!("Key {}", self.key)).call().map_err(describe)?;
        let value: serde_json::Value = serde_json::from_str(&result.body_mut().with_config().limit(64 << 20).read_to_string().context("reading the result")?).context("the result is not JSON")?;
        if let Some(error) = value.get("error").or(value.get("detail")) { bail!("fal: {error}"); }
        let images: Vec<serde_json::Value> = match (value.get("images").and_then(|v| v.as_array()), value.get("image")) {
            (Some(list), _) => list.clone(),
            (None, Some(one)) => vec![one.clone()],
            _ => bail!("fal returned no images"),
        };
        let mut out = Vec::new();
        for (i, image) in images.iter().enumerate() {
            let url = image.get("url").and_then(|v| v.as_str()).context("an image has no URL")?;
            progress(&format!("Fetching result {} of {}…", i + 1, images.len()));
            let bytes = if let Some(rest) = url.strip_prefix("data:") {
                let comma = rest.find(',').context("bad data URI")?;
                base64_decode(&rest[comma + 1..])?
            } else {
                let mut r = ureq::get(url).call().map_err(describe)?;
                r.body_mut().with_config().limit(64 << 20).read_to_vec().context("downloading the image")?
            };
            out.push(bytes);
        }
        Ok(out)
    }
}

fn describe(error: ureq::Error) -> anyhow::Error {
    match error {
        ureq::Error::StatusCode(401) | ureq::Error::StatusCode(403) => anyhow::anyhow!("fal refused the key (HTTP {}). Check ~/.config/compositor/fal.key.", match error { ureq::Error::StatusCode(c) => c, _ => 0 }),
        ureq::Error::StatusCode(422) => anyhow::anyhow!("fal rejected the request (HTTP 422): the model did not accept these inputs."),
        ureq::Error::StatusCode(code) => anyhow::anyhow!("fal answered HTTP {code}."),
        other => anyhow::anyhow!("network: {other}"),
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

pub fn base64_decode(text: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        let v = match c { b'A'..=b'Z' => c - b'A', b'a'..=b'z' => c - b'a' + 26, b'0'..=b'9' => c - b'0' + 52, b'+' => 62, b'/' => 63, b'=' | b'\n' | b'\r' | b' ' => continue, _ => bail!("bad base64") } as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 { bits -= 8; out.push((acc >> bits) as u8); acc &= (1 << bits) - 1; }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn base64_round_trips_and_the_body_has_the_fields() {
        for data in [&b""[..], b"f", b"fo", b"foo", b"\x00\xff\x10\x80"] { assert_eq!(base64_decode(&base64_encode(data)).unwrap(), data); }
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
        let r = Request { model: default_models()[0].clone(), prompt: "".into(), count: 9, seed: Some(7), image_png: vec![1, 2], mask_png: vec![3] };
        let b = body(&r);
        assert!(b["image_url"].as_str().unwrap().starts_with("data:image/png;base64,"));
        assert_eq!(b["num_images"], 4);
        assert_eq!(b["seed"], 7);
        assert!(!b["prompt"].as_str().unwrap().is_empty(), "an empty prompt still asks for something");
    }
}
