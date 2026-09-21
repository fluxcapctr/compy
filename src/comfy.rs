//! A local ComfyUI instance as a generation backend: the same tools that go to fal can instead run
//! against a model on this machine, so nothing leaves it and nothing is billed. ComfyUI's API is
//! shaped like fal's queue (submit a job, poll, fetch), so this mirrors `genfill::Fal` closely.
//!
//! The graphs built here target Qwen-Image-2.1: a single-stream DiT with a Qwen3-VL-8B text encoder
//! and an RGBA VAE. `CLIPLoader` takes `qwen_image` as its type and picks the 2.1 encoder from the
//! weights themselves.

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

use crate::genfill::{Backend, Request, base64_encode};

/// How long to wait between polls of the history endpoint.
const POLL_MS: u64 = 400;

fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".config"));
    base.join("compositor")
}

/// Which local weights the graphs load, from `~/.config/compositor/comfy.json` (written with the
/// defaults when absent). The names are ComfyUI's, relative to its `models/` subfolders.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Where ComfyUI listens.
    #[serde(default = "default_host")]
    pub host: String,
    /// `models/diffusion_models/…`
    #[serde(default = "default_unet")]
    pub unet: String,
    /// `models/text_encoders/…`
    #[serde(default = "default_clip")]
    pub clip: String,
    /// `models/vae/…`
    #[serde(default = "default_vae")]
    pub vae: String,
    #[serde(default = "default_steps")]
    pub steps: u32,
    #[serde(default = "default_cfg")]
    pub cfg: f64,
    #[serde(default = "default_sampler")]
    pub sampler: String,
    #[serde(default = "default_scheduler")]
    pub scheduler: String,
}

fn default_host() -> String { "http://127.0.0.1:8188".into() }
fn default_unet() -> String { "qwen_image_2.1_int8_convrot.safetensors".into() }
fn default_clip() -> String { "qwen3vl_8b_int8_convrot.safetensors".into() }
fn default_vae() -> String { "qwen_image_2.1_vae_bf16.safetensors".into() }
fn default_steps() -> u32 { 20 }
fn default_cfg() -> f64 { 2.5 }
fn default_sampler() -> String { "euler".into() }
fn default_scheduler() -> String { "simple".into() }

impl Default for Config {
    fn default() -> Self {
        Self { host: default_host(), unet: default_unet(), clip: default_clip(), vae: default_vae(), steps: default_steps(), cfg: default_cfg(), sampler: default_sampler(), scheduler: default_scheduler() }
    }
}

/// The local config, from `~/.config/compositor/comfy.json`, written with the defaults when absent.
/// `COMPOSITOR_COMFY_URL` overrides the host, so a one-off run can point somewhere else.
pub fn config() -> Config {
    let path = config_dir().join("comfy.json");
    let mut c = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Config>(&t).ok()).unwrap_or_else(|| {
        let c = Config::default();
        if let Some(dir) = path.parent() { let _ = std::fs::create_dir_all(dir); }
        let _ = std::fs::write(&path, serde_json::to_string_pretty(&c).unwrap_or_default());
        c
    });
    if let Ok(h) = std::env::var("COMPOSITOR_COMFY_URL") { let h = h.trim().to_string(); if !h.is_empty() { c.host = h; } }
    c
}

/// True when a ComfyUI is actually listening, so the UI can offer the local backend only when it
/// would work. Cheap: a single request to the root endpoint with a short timeout.
pub fn available(host: &str) -> bool {
    ureq::get(&format!("{}/system_stats", host.trim_end_matches('/'))).call().is_ok()
}

pub struct Comfy {
    pub config: Config,
    /// Identifies this client to ComfyUI so its queue and history stay ours.
    pub client_id: String,
}

impl Comfy {
    pub fn new() -> Self {
        Self { config: config(), client_id: format!("compositor-{}", std::process::id()) }
    }

    fn base(&self) -> &str { self.config.host.trim_end_matches('/') }

    /// Puts a PNG in ComfyUI's input folder and returns the name a `LoadImage` node should use.
    /// Multipart by hand: ureq has no form builder and the body is one small part.
    fn upload(&self, png: &[u8], name: &str) -> Result<String> {
        let boundary = format!("----compositor{}", std::process::id());
        let mut body = Vec::new();
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"{name}\"\r\nContent-Type: image/png\r\n\r\n").as_bytes());
        body.extend_from_slice(png);
        body.extend_from_slice(format!("\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"overwrite\"\r\n\r\ntrue\r\n--{boundary}--\r\n").as_bytes());
        let mut r = ureq::post(&format!("{}/upload/image", self.base()))
            .header("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
            .send(&body[..]).map_err(describe)?;
        let v: Value = serde_json::from_str(&r.body_mut().read_to_string().context("reading ComfyUI's upload reply")?).context("ComfyUI's upload reply is not JSON")?;
        let uploaded = v.get("name").and_then(Value::as_str).context("ComfyUI gave no name for the upload")?;
        let subfolder = v.get("subfolder").and_then(Value::as_str).unwrap_or("");
        Ok(if subfolder.is_empty() { uploaded.to_string() } else { format!("{subfolder}/{uploaded}") })
    }

    /// Submits a graph and returns its prompt id.
    fn submit(&self, graph: Value) -> Result<String> {
        let body = json!({"prompt": graph, "client_id": self.client_id}).to_string();
        let mut r = ureq::post(&format!("{}/prompt", self.base())).header("Content-Type", "application/json").send(body.as_bytes()).map_err(describe)?;
        let text = r.body_mut().read_to_string().context("reading ComfyUI's reply")?;
        let v: Value = serde_json::from_str(&text).with_context(|| format!("ComfyUI's reply is not JSON: {}", text.chars().take(200).collect::<String>()))?;
        // A graph ComfyUI will not run comes back with the offending node named; surface that.
        if let Some(error) = v.get("error") {
            let message = error.get("message").and_then(Value::as_str).unwrap_or("ComfyUI rejected the graph");
            let details = v.get("node_errors").map(|n| n.to_string()).unwrap_or_default();
            bail!("{message}{}", if details.is_empty() || details == "{}" { String::new() } else { format!(" ({details})") });
        }
        Ok(v.get("prompt_id").and_then(Value::as_str).context("ComfyUI gave no prompt id")?.to_string())
    }

    /// Polls until the graph finishes, then downloads every image it produced.
    fn wait(&self, prompt_id: &str, progress: &dyn Fn(&str), cancelled: &dyn Fn() -> bool) -> Result<Vec<Vec<u8>>> {
        let started = std::time::Instant::now();
        loop {
            if cancelled() {
                let _ = ureq::post(&format!("{}/interrupt", self.base())).send_empty();
                bail!("cancelled");
            }
            let mut r = ureq::get(&format!("{}/history/{prompt_id}", self.base())).call().map_err(describe)?;
            let v: Value = serde_json::from_str(&r.body_mut().read_to_string().context("reading ComfyUI's history")?).context("ComfyUI's history is not JSON")?;
            if let Some(entry) = v.get(prompt_id) {
                let status = entry.get("status");
                let done = status.and_then(|s| s.get("completed")).and_then(Value::as_bool).unwrap_or(false);
                if let Some("error") = status.and_then(|s| s.get("status_str")).and_then(Value::as_str) {
                    let why = entry.get("status").and_then(|s| s.get("messages")).map(|m| m.to_string()).unwrap_or_default();
                    bail!("ComfyUI failed to run the graph{}", if why.is_empty() { String::new() } else { format!(": {why}") });
                }
                if done { return self.collect(entry); }
            }
            progress(&format!("Generating locally… {}s", started.elapsed().as_secs()));
            std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
        }
    }

    /// Every image named in a finished history entry, downloaded as PNG bytes.
    fn collect(&self, entry: &Value) -> Result<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        let outputs = entry.get("outputs").and_then(Value::as_object).context("ComfyUI's result has no outputs")?;
        for node in outputs.values() {
            let Some(images) = node.get("images").and_then(Value::as_array) else { continue };
            for image in images {
                let filename = image.get("filename").and_then(Value::as_str).unwrap_or_default();
                if filename.is_empty() { continue }
                let subfolder = image.get("subfolder").and_then(Value::as_str).unwrap_or("");
                let kind = image.get("type").and_then(Value::as_str).unwrap_or("output");
                // Temp previews are not results; only saved output counts.
                if kind == "temp" { continue }
                let url = format!("{}/view?filename={}&subfolder={}&type={}", self.base(), urlencode(filename), urlencode(subfolder), urlencode(kind));
                let mut r = ureq::get(&url).call().map_err(describe)?;
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut r.body_mut().as_reader(), &mut bytes).context("downloading an image from ComfyUI")?;
                out.push(bytes);
            }
        }
        if out.is_empty() { bail!("ComfyUI finished but produced no image; the graph may have no SaveImage node") }
        Ok(out)
    }

    /// Runs one graph end to end.
    pub fn run_graph(&self, graph: Value, progress: &dyn Fn(&str), cancelled: &dyn Fn() -> bool) -> Result<Vec<Vec<u8>>> {
        progress("Sending to ComfyUI…");
        let id = self.submit(graph)?;
        self.wait(&id, progress, cancelled)
    }

    /// The three loaders every Qwen-Image-2.1 graph starts with, as nodes "1" (unet), "2" (clip) and
    /// "3" (vae).
    fn loaders(&self) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        m.insert("1".into(), json!({"class_type": "UNETLoader", "inputs": {"unet_name": self.config.unet, "weight_dtype": "default"}}));
        m.insert("2".into(), json!({"class_type": "CLIPLoader", "inputs": {"clip_name": self.config.clip, "type": "qwen_image"}}));
        m.insert("3".into(), json!({"class_type": "VAELoader", "inputs": {"vae_name": self.config.vae}}));
        m
    }

    /// Sampler, decode and save, as nodes "7", "8" and "9", reading conditioning from `positive` and
    /// `negative` and pixels from `latent`.
    fn tail(&self, m: &mut serde_json::Map<String, Value>, positive: Value, negative: Value, latent: Value, seed: u64) {
        m.insert("7".into(), json!({"class_type": "KSampler", "inputs": {
            "model": ["1", 0], "positive": positive, "negative": negative, "latent_image": latent,
            "seed": seed, "steps": self.config.steps, "cfg": self.config.cfg,
            "sampler_name": self.config.sampler, "scheduler": self.config.scheduler, "denoise": 1.0,
        }}));
        m.insert("8".into(), json!({"class_type": "VAEDecode", "inputs": {"samples": ["7", 0], "vae": ["3", 0]}}));
        m.insert("9".into(), json!({"class_type": "SaveImage", "inputs": {"images": ["8", 0], "filename_prefix": "compositor"}}));
    }

    /// Text to image at `width` x `height`. `layers` above zero asks the RGBA VAE for a real alpha
    /// channel, which is what Compy's `transparent` means.
    pub fn text_to_image(&self, prompt: &str, negative: &str, width: usize, height: usize, transparent: bool, seed: u64) -> Value {
        let mut m = self.loaders();
        m.insert("4".into(), json!({"class_type": "TextEncodeQwenImage21", "inputs": {
            "clip": ["2", 0], "prompt": prompt, "negative_prompt": negative, "vae": ["3", 0], "resolution": 1024,
        }}));
        // Sizes are latent-aligned; the caller's request is honoured to the nearest multiple of 16.
        let (w, h) = (round16(width), round16(height));
        m.insert("5".into(), json!({"class_type": "EmptyQwenImageLayeredLatentImage", "inputs": {
            "width": w, "height": h, "layers": if transparent { 3 } else { 0 }, "batch_size": 1,
        }}));
        self.tail(&mut m, json!(["4", 0]), json!(["4", 1]), json!(["5", 0]), seed);
        Value::Object(m)
    }

    /// An edit of `source_name` (already uploaded): the reference image goes to the text encoder,
    /// which also hands back the latent sized to match, so the edit does not shift.
    pub fn edit(&self, prompt: &str, negative: &str, source_name: &str, seed: u64) -> Value {
        let mut m = self.loaders();
        m.insert("6".into(), json!({"class_type": "LoadImage", "inputs": {"image": source_name}}));
        m.insert("4".into(), json!({"class_type": "TextEncodeQwenImage21", "inputs": {
            "clip": ["2", 0], "prompt": prompt, "negative_prompt": negative, "vae": ["3", 0], "resolution": 1024,
            "image_1": ["6", 0],
        }}));
        // The encoder's third output is an empty latent on the reference's size.
        self.tail(&mut m, json!(["4", 0]), json!(["4", 1]), json!(["4", 2]), seed);
        Value::Object(m)
    }

    /// Masked inpainting: the context window and its mask become a latent the sampler only touches
    /// where the mask is white, which is the shape Generative Fill and Expand ask for.
    pub fn inpaint(&self, prompt: &str, negative: &str, image_name: &str, mask_name: &str, seed: u64) -> Value {
        let mut m = self.loaders();
        m.insert("6".into(), json!({"class_type": "LoadImage", "inputs": {"image": image_name}}));
        m.insert("10".into(), json!({"class_type": "LoadImage", "inputs": {"image": mask_name}}));
        // White marks what to paint, and LoadImage's alpha-derived mask is inverted from that, so the
        // mask comes from the red channel of a greyscale PNG instead.
        m.insert("11".into(), json!({"class_type": "ImageToMask", "inputs": {"image": ["10", 0], "channel": "red"}}));
        m.insert("4".into(), json!({"class_type": "TextEncodeQwenImage21", "inputs": {
            "clip": ["2", 0], "prompt": prompt, "negative_prompt": negative, "vae": ["3", 0], "resolution": 1024,
            "image_1": ["6", 0],
        }}));
        m.insert("12".into(), json!({"class_type": "VAEEncodeForInpaint", "inputs": {
            "pixels": ["6", 0], "vae": ["3", 0], "mask": ["11", 0], "grow_mask_by": 6,
        }}));
        self.tail(&mut m, json!(["4", 0]), json!(["4", 1]), json!(["12", 0]), seed);
        Value::Object(m)
    }
}

impl Default for Comfy {
    fn default() -> Self { Self::new() }
}

impl Comfy {
    /// The agent tools hand fal-shaped bodies around (`genfill::agent_body`); this reads one and runs
    /// the matching local graph. `image_url` means an edit of that image, `image_size` a generation
    /// at that size. `upscale` and `relight` never arrive here: their ids are fal's.
    pub fn run_agent(&self, body: &Value, transparent: bool, progress: &dyn Fn(&str), cancelled: &dyn Fn() -> bool) -> Result<Vec<Vec<u8>>> {
        let prompt = body.get("prompt").and_then(Value::as_str).unwrap_or_default();
        let negative = body.get("negative_prompt").and_then(Value::as_str).unwrap_or_default();
        let count = body.get("num_images").and_then(Value::as_u64).unwrap_or(1).clamp(1, 4) as usize;
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(1);

        // An edit carries its source either singly or as fal's one-element list.
        let source = body.get("image_url").and_then(Value::as_str)
            .or_else(|| body.get("image_urls").and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_str));
        let uploaded = match source {
            Some(uri) => {
                progress("Uploading to ComfyUI…");
                let png = from_data_uri(uri).context("the source image is not a data URI ComfyUI can take")?;
                Some(self.upload(&png, &format!("compositor-{stamp}-source.png"))?)
            }
            None => None,
        };

        let mut out = Vec::new();
        for i in 0..count {
            if cancelled() { break }
            if count > 1 { progress(&format!("Asking for variation {} of {count}…", i + 1)); }
            let seed = stamp.wrapping_add(i as u64 * 7919);
            let graph = match &uploaded {
                Some(name) => self.edit(prompt, negative, name, seed),
                None => {
                    let size = body.get("image_size");
                    let width = size.and_then(|s| s.get("width")).and_then(Value::as_u64).unwrap_or(1024) as usize;
                    let height = size.and_then(|s| s.get("height")).and_then(Value::as_u64).unwrap_or(1024) as usize;
                    self.text_to_image(prompt, negative, width, height, transparent, seed)
                }
            };
            out.extend(self.run_graph(graph, progress, cancelled)?);
        }
        Ok(out)
    }
}

/// Generative Fill and Expand: upload the window and its mask, then inpaint.
impl Backend for Comfy {
    fn generate(&self, request: &Request, progress: &dyn Fn(&str), cancelled: &dyn Fn() -> bool) -> Result<Vec<Vec<u8>>> {
        let wanted = request.count.clamp(1, 4) as usize;
        progress("Uploading to ComfyUI…");
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let image = self.upload(&request.image_png, &format!("compositor-{stamp}-image.png"))?;
        let mask = self.upload(&request.mask_png, &format!("compositor-{stamp}-mask.png"))?;
        let prompt = if request.prompt.trim().is_empty() { "fill in naturally, matching the surroundings" } else { &request.prompt };
        let mut out = Vec::new();
        // One graph per variation: ComfyUI batches by latent, but a fresh seed each time matches how
        // the fal backend produces variations.
        for i in 0..wanted {
            if cancelled() { break }
            if wanted > 1 { progress(&format!("Asking for variation {} of {wanted}…", i + 1)); }
            let seed = request.seed.unwrap_or(stamp as u64).wrapping_add(i as u64 * 7919);
            let graph = self.inpaint(prompt, "", &image, &mask, seed);
            out.extend(self.run_graph(graph, progress, cancelled)?);
        }
        Ok(out)
    }
}

fn round16(n: usize) -> usize { ((n + 8) / 16).max(1) * 16 }

fn urlencode(s: &str) -> String {
    s.bytes().map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
        _ => format!("%{b:02X}"),
    }).collect()
}

/// ureq's errors name the host but not what Compy was doing; say both.
fn describe(e: ureq::Error) -> anyhow::Error {
    anyhow::anyhow!("could not reach ComfyUI: {e}")
}

/// A data URI back to raw PNG bytes, for callers that hold fal-shaped sources.
pub fn from_data_uri(uri: &str) -> Result<Vec<u8>> {
    let rest = uri.strip_prefix("data:").and_then(|r| r.split_once(",")).map(|(_, b)| b).context("not a data URI")?;
    crate::genfill::base64_decode(rest)
}

/// PNG bytes as a data URI, matching `genfill::data_uri`.
pub fn to_data_uri(png: &[u8]) -> String { format!("data:image/png;base64,{}", base64_encode(png)) }

#[cfg(test)]
mod tests {
    use super::*;

    fn comfy() -> Comfy { Comfy { config: Config::default(), client_id: "test".into() } }

    #[test]
    fn text_to_image_graph_is_wired() {
        let g = comfy().text_to_image("a cat", "", 1024, 1024, false, 7);
        assert_eq!(g["2"]["inputs"]["type"], "qwen_image");
        assert_eq!(g["5"]["inputs"]["layers"], 0);
        assert_eq!(g["7"]["inputs"]["latent_image"][0], "5");
        assert_eq!(g["7"]["inputs"]["seed"], 7);
        assert_eq!(g["9"]["class_type"], "SaveImage");
    }

    #[test]
    fn transparent_asks_the_rgba_vae_for_layers() {
        let g = comfy().text_to_image("a logo", "", 512, 512, true, 1);
        assert_eq!(g["5"]["inputs"]["layers"], 3);
    }

    #[test]
    fn edit_takes_its_latent_from_the_encoder() {
        let g = comfy().edit("make it night", "", "in.png", 3);
        assert_eq!(g["4"]["inputs"]["image_1"][0], "6");
        assert_eq!(g["7"]["inputs"]["latent_image"], json!(["4", 2]));
    }

    #[test]
    fn inpaint_masks_from_the_red_channel() {
        let g = comfy().inpaint("a tree", "", "in.png", "mask.png", 5);
        assert_eq!(g["11"]["inputs"]["channel"], "red");
        assert_eq!(g["12"]["class_type"], "VAEEncodeForInpaint");
        assert_eq!(g["7"]["inputs"]["latent_image"], json!(["12", 0]));
    }

    #[test]
    fn sizes_round_to_the_latent_grid() {
        assert_eq!(round16(1023), 1024);
        assert_eq!(round16(1000), 1008);
        assert_eq!(round16(1), 16);
    }

    #[test]
    fn urlencode_escapes_what_it_must() {
        assert_eq!(urlencode("a b/c.png"), "a%20b%2Fc.png");
        assert_eq!(urlencode("plain-name_1.png"), "plain-name_1.png");
    }
}
