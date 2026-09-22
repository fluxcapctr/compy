import React from "react";
import {
  AbsoluteFill, Composition, Img, OffthreadVideo, Sequence, staticFile,
  interpolate, spring, useCurrentFrame, useVideoConfig, Easing,
} from "remotion";

// Palette sampled from the app under the Omarchy theme the footage was shot in.
const BG = "#010a14";
const PANEL = "#021321";
const INK = "#e8eef4";
const MUTED = "#8ea0b4";
const ACCENT = "#e5a75a";
const SANS = "'Adwaita Sans', 'Noto Sans', sans-serif";
const MONO = "'JetBrainsMono Nerd Font', 'JetBrains Mono', 'Liberation Mono', monospace";

const FPS = 30;
const SRC_W = 2350, SRC_H = 1400;
const FIT = 1080 / SRC_H;          // footage fitted to the frame height
const FIT_W = SRC_W * FIT;         // 1813
const FIT_LEFT = (1920 - FIT_W) / 2;

type Zoom = { start: number; end: number; x: number; y: number; z: number };
type Part = { from: number; to: number; rate?: number };
type Scene = {
  key: string;
  parts: Part[];                   // footage ranges in source seconds, played back to back
  label: string;                   // small mono eyebrow
  title: string;                   // big line
  sub?: string;                    // one quiet line under the title
  zoom?: Zoom;                     // punch in, times in scene seconds
  captionAt?: number;              // seconds into the scene when the caption appears
};

const partLen = (p: Part) => (p.to - p.from) / (p.rate ?? 1);
const sceneLen = (s: Scene) => s.parts.reduce((a, p) => a + partLen(p), 0);

// Scene marks come from the capture log (source seconds).
const SCENES: Scene[] = [
  { key: "open", parts: [{ from: 3.4, to: 6.9, rate: 1.3 }],
    label: "Open", title: "Open a photo.", sub: "PSD, PNG, JPEG, HEIC, WebP, TIFF.", captionAt: 0.6 },
  { key: "curves", parts: [{ from: 7.06, to: 12.9, rate: 1.2 }],
    label: "Image", title: "Curves on a selection.", sub: "Levels, Curves, Hue and Saturation, Color Balance, Shadows and Highlights.",
    zoom: { start: 1.3, end: 4.4, x: 1160, y: 690, z: 1.55 } },
  { key: "adjust", parts: [{ from: 13.07, to: 15.7, rate: 1.1 }],
    label: "Layers", title: "Adjustment layers.", sub: "Vibrance and Photo Filter, live and non destructive." },
  { key: "style", parts: [{ from: 15.73, to: 21.8, rate: 1.25 }],
    label: "Layers", title: "Shapes and layer styles.", sub: "Drop shadow, glow, bevel, stroke, overlays, Blend If.",
    zoom: { start: 1.4, end: 3.7, x: 1160, y: 690, z: 1.5 } },
  { key: "type", parts: [{ from: 21.92, to: 27.4, rate: 1.25 }],
    label: "Type", title: "Type stays editable.", sub: "Restyle, recolor, resize. Never rasterized until you say so.",
    zoom: { start: 0.6, end: 4.2, x: 1000, y: 1150, z: 1.6 } },
  { key: "brush", parts: [{ from: 27.47, to: 30.8, rate: 1.15 }],
    label: "Paint", title: "Brushes with real tips.", sub: "Hardness, spacing, jitter, roundness. Photoshop ABR files load as is." },
  { key: "blur", parts: [{ from: 30.84, to: 37.4, rate: 1.3 }],
    label: "Filter", title: "Filters and blend modes.", sub: "Gaussian, motion, radial, sharpen, high pass, noise and more.",
    zoom: { start: 1.3, end: 3.5, x: 1160, y: 675, z: 1.7 } },
  { key: "removebg", parts: [{ from: 37.56, to: 43.3, rate: 1.35 }],
    label: "AI, local", title: "Background removal, on your machine.", sub: "A local ONNX model. Nothing leaves the computer." },
  { key: "history", parts: [{ from: 43.44, to: 50.6, rate: 1.45 }],
    label: "History", title: "Undo anything. Jump anywhere.", sub: "Every step is a snapshot you can go back to.",
    zoom: { start: 3.3, end: 4.9, x: 1160, y: 690, z: 1.6 } },
  { key: "assistant",
    parts: [{ from: 50.73, to: 57.5 }, { from: 57.5, to: 79.8, rate: 12 }, { from: 79.8, to: 86.6 }],
    label: "Agent", title: "Ask Compy in plain words.", sub: "Runs on Claude Code, the same agent you use with Omarchy. It picks the tools.",
    zoom: { start: 9.4, end: 14.6, x: 1220, y: 288, z: 1.7 } },
  { key: "genfill", parts: [{ from: 87.92, to: 92.6 }, { from: 140.6, to: 146.4 }],
    label: "Generative", title: "Generative fill, expand and edit.", sub: "Through fal.ai with your own key, or a local ComfyUI. Your choice.",
    zoom: { start: 5.6, end: 10.4, x: 808, y: 690, z: 1.7 } },
  { key: "export", parts: [{ from: 146.6, to: 158.6, rate: 1.3 }],
    label: "Export", title: "Every size at once.", sub: "Instagram, Story, X and more as artboards, reframed, styles scaled, exported in one go.",
    zoom: { start: 0.9, end: 2.5, x: 1150, y: 610, z: 1.4 } },
];

const INTRO = 3.3, THEMES = 4.2, OUTRO = 5.0;
const TOTAL = INTRO + SCENES.reduce((a, s) => a + sceneLen(s), 0) + THEMES + OUTRO;

const PLATE: React.CSSProperties = { position: "absolute", left: 60, bottom: 64, maxWidth: 620,
  padding: "22px 28px 24px 28px", borderRadius: 10, background: "rgba(1,10,20,0.78)",
  backdropFilter: "blur(14px)", WebkitBackdropFilter: "blur(14px)", border: "1px solid rgba(255,255,255,0.07)",
  boxShadow: "0 12px 40px rgba(0,0,0,0.45)" };

const ease = Easing.bezier(0.2, 0.8, 0.2, 1);

const Footage: React.FC<{ scene: Scene }> = ({ scene }) => {
  const frame = useCurrentFrame();
  const t = frame / FPS;
  const z = scene.zoom;
  let scale = 1;
  if (z) {
    const k = interpolate(t, [z.start, z.start + 0.7, z.end - 0.55, z.end], [0, 1, 1, 0],
      { extrapolateLeft: "clamp", extrapolateRight: "clamp", easing: ease });
    scale = 1 + (z.z - 1) * k;
  }
  const origin = z ? `${z.x * FIT}px ${z.y * FIT}px` : "center";
  let at = 0;
  return (
    <AbsoluteFill style={{ background: BG }}>
      <div style={{ position: "absolute", left: FIT_LEFT, top: 0, width: FIT_W, height: 1080,
        transform: `scale(${scale})`, transformOrigin: origin }}>
        {scene.parts.map((p, i) => {
          const start = Math.round(at * FPS);
          const dur = Math.max(1, Math.round(partLen(p) * FPS));
          at += partLen(p);
          return (
            <Sequence key={i} from={start} durationInFrames={dur} layout="none" premountFor={20}>
              <OffthreadVideo src={staticFile("session.mp4")} muted
                startFrom={Math.round(p.from * FPS)} playbackRate={p.rate ?? 1}
                style={{ width: FIT_W, height: 1080, display: "block" }} />
              {(p.rate ?? 1) >= 4 ? <SpeedTag rate={p.rate!} /> : null}
            </Sequence>
          );
        })}
      </div>
    </AbsoluteFill>
  );
};

const SpeedTag: React.FC<{ rate: number }> = ({ rate }) => {
  const frame = useCurrentFrame();
  const o = interpolate(frame, [0, 6], [0, 1], { extrapolateRight: "clamp" });
  return (
    <div style={{ position: "absolute", right: 420, top: 36, opacity: o, fontFamily: MONO, fontSize: 22,
      color: BG, background: ACCENT, padding: "6px 14px", borderRadius: 6, letterSpacing: 1 }}>
      {rate}× · the agent is working
    </div>
  );
};

const Caption: React.FC<{ scene: Scene; total: number }> = ({ scene, total }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  const start = Math.round((scene.captionAt ?? 0.25) * fps);
  const inS = spring({ frame: frame - start, fps, config: { damping: 18, stiffness: 120 } });
  const out = interpolate(frame, [total - 10, total - 2], [1, 0], { extrapolateLeft: "clamp", extrapolateRight: "clamp" });
  const o = Math.min(inS, out);
  const y = (1 - inS) * 28;
  return (
    <div style={{ ...PLATE, opacity: o, transform: `translateY(${y}px)` }}>
      <div style={{ display: "flex", alignItems: "center", gap: 10, marginBottom: 12 }}>
        <div style={{ width: 10, height: 10, background: ACCENT, borderRadius: 2 }} />
        <div style={{ fontFamily: MONO, fontSize: 20, color: ACCENT, letterSpacing: 2.5, textTransform: "uppercase" }}>{scene.label}</div>
      </div>
      <div style={{ fontFamily: SANS, fontWeight: 700, fontSize: 46, lineHeight: 1.08, color: INK, letterSpacing: -1,
        }}>{scene.title}</div>
      {scene.sub ? (
        <div style={{ fontFamily: SANS, fontSize: 24, lineHeight: 1.3, color: MUTED, marginTop: 10 }}>{scene.sub}</div>
      ) : null}
    </div>
  );
};

const Word: React.FC<{ text: string; delay: number; size: number; color?: string; weight?: number; mono?: boolean }> =
  ({ text, delay, size, color = INK, weight = 700, mono }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  const s = spring({ frame: frame - delay, fps, config: { damping: 16, stiffness: 110 } });
  return (
    <div style={{ fontFamily: mono ? MONO : SANS, fontWeight: weight, fontSize: size, color, letterSpacing: mono ? 1 : -size * 0.03,
      opacity: s, transform: `translateY(${(1 - s) * 30}px)`, lineHeight: 1.05 }}>{text}</div>
  );
};

const Intro: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps, durationInFrames } = useVideoConfig();
  const mark = spring({ frame, fps, config: { damping: 12, stiffness: 90 } });
  const out = interpolate(frame, [durationInFrames - 8, durationInFrames], [1, 0], { extrapolateLeft: "clamp" });
  return (
    <AbsoluteFill style={{ background: BG, justifyContent: "center", alignItems: "center", opacity: out }}>
      <div style={{ position: "absolute", inset: 0,
        background: `radial-gradient(ellipse 60% 55% at 50% 45%, ${PANEL} 0%, ${BG} 70%)` }} />
      <div style={{ display: "flex", alignItems: "center", gap: 34, transform: `scale(${0.7 + mark * 0.3})`, opacity: mark }}>
        <Img src={staticFile("mark-glyph.png")} style={{ height: 124 }} />
        <div style={{ fontFamily: SANS, fontWeight: 700, fontSize: 132, color: INK, letterSpacing: -6, lineHeight: 1 }}>Compy</div>
      </div>
      <div style={{ marginTop: 44, textAlign: "center" }}>
        <Word text="A Photoshop for Omarchy." delay={14} size={50} />
        <div style={{ height: 10 }} />
        <Word text="All the tools. None of the bloat." delay={26} size={34} color={MUTED} weight={400} />
      </div>
    </AbsoluteFill>
  );
};

const THEME_STILLS = [
  { file: "theme_catppuccin.png", name: "Catppuccin" },
  { file: "theme_gruvbox.png", name: "Gruvbox" },
  { file: "theme_kanagawa.png", name: "Kanagawa" },
];

const Themes: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps, durationInFrames } = useVideoConfig();
  const each = durationInFrames / THEME_STILLS.length;
  const idx = Math.min(THEME_STILLS.length - 1, Math.floor(frame / each));
  const fade = 8;
  return (
    <AbsoluteFill style={{ background: BG }}>
      {THEME_STILLS.map((t, i) => {
        const a = i * each, b = (i + 1) * each;
        const o = i === 0
          ? interpolate(frame, [b - fade, b], [1, 0], { extrapolateLeft: "clamp", extrapolateRight: "clamp" })
          : interpolate(frame, [a - fade, a, b - fade, b], [0, 1, 1, i === THEME_STILLS.length - 1 ? 1 : 0], { extrapolateLeft: "clamp", extrapolateRight: "clamp" });
        const drift = interpolate(frame, [a - fade, b], [1.0, 1.03], { extrapolateLeft: "clamp", extrapolateRight: "clamp" });
        return (
          <AbsoluteFill key={t.file} style={{ opacity: o, justifyContent: "center", alignItems: "center" }}>
            <Img src={staticFile(t.file)} style={{ height: 1080, transform: `scale(${drift})` }} />
          </AbsoluteFill>
        );
      })}
      <div style={PLATE}>
        <div style={{ display: "flex", alignItems: "center", gap: 10, marginBottom: 12 }}>
          <div style={{ width: 10, height: 10, background: ACCENT, borderRadius: 2 }} />
          <div style={{ fontFamily: MONO, fontSize: 20, color: ACCENT, letterSpacing: 2.5, textTransform: "uppercase" }}>Omarchy</div>
        </div>
        <div style={{ fontFamily: SANS, fontWeight: 700, fontSize: 46, color: INK, letterSpacing: -1}}>
          Follows your theme.
        </div>
        <div style={{ fontFamily: MONO, fontSize: 24, color: MUTED, marginTop: 10}}>
          {THEME_STILLS[idx].name} · switches live with omarchy theme set
        </div>
      </div>
    </AbsoluteFill>
  );
};

const Outro: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  const s = spring({ frame, fps, config: { damping: 14, stiffness: 100 } });
  const cmd = spring({ frame: frame - 16, fps, config: { damping: 16, stiffness: 120 } });
  return (
    <AbsoluteFill style={{ background: BG, justifyContent: "center", alignItems: "center" }}>
      <div style={{ position: "absolute", inset: 0,
        background: `radial-gradient(ellipse 60% 55% at 50% 40%, ${PANEL} 0%, ${BG} 70%)` }} />
      <div style={{ display: "flex", alignItems: "center", gap: 26, opacity: s, transform: `translateY(${(1 - s) * 20}px)` }}>
        <Img src={staticFile("mark-glyph.png")} style={{ height: 86 }} />
        <div style={{ fontFamily: SANS, fontWeight: 700, fontSize: 92, color: INK, letterSpacing: -4 }}>Compy</div>
      </div>
      <div style={{ marginTop: 22, opacity: s, fontFamily: SANS, fontSize: 32, color: MUTED }}>
        Free and open source. Built for Omarchy, runs on any Linux.
      </div>
      <div style={{ marginTop: 46, opacity: cmd, transform: `translateY(${(1 - cmd) * 20}px)`,
        fontFamily: MONO, fontSize: 30, color: INK, background: "rgba(255,255,255,0.05)",
        border: `1px solid rgba(255,255,255,0.12)`, borderLeft: `4px solid ${ACCENT}`, padding: "20px 34px", borderRadius: 8 }}>
        curl -fsSL https://raw.githubusercontent.com/fluxcapctr/compy/main/get.sh | bash
      </div>
      <div style={{ marginTop: 30, opacity: cmd, fontFamily: MONO, fontSize: 28, color: ACCENT, letterSpacing: 1 }}>
        github.com/fluxcapctr/compy
      </div>
    </AbsoluteFill>
  );
};

const Launch: React.FC = () => {
  let at = INTRO;
  const cuts = SCENES.map((s) => { const start = at; at += sceneLen(s); return { s, start }; });
  const themesAt = at; at += THEMES;
  const outroAt = at;
  return (
    <AbsoluteFill style={{ background: BG }}>
      <Sequence from={0} durationInFrames={Math.round(INTRO * FPS)}><Intro /></Sequence>
      {cuts.map(({ s, start }) => {
        const dur = Math.round(sceneLen(s) * FPS);
        return (
          <Sequence key={s.key} from={Math.round(start * FPS)} durationInFrames={dur} premountFor={20}>
            <Footage scene={s} />
            <Caption scene={s} total={dur} />
          </Sequence>
        );
      })}
      <Sequence from={Math.round(themesAt * FPS)} durationInFrames={Math.round(THEMES * FPS)}><Themes /></Sequence>
      <Sequence from={Math.round(outroAt * FPS)} durationInFrames={Math.round(OUTRO * FPS)}><Outro /></Sequence>
    </AbsoluteFill>
  );
};

export const Root: React.FC = () => (
  <Composition id="Launch" component={Launch} width={1920} height={1080} fps={FPS}
    durationInFrames={Math.round(TOTAL * FPS)} />
);
