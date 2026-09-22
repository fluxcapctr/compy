import React from "react";
import {
  AbsoluteFill, Audio, Composition, Img, OffthreadVideo, Sequence, continueRender, delayRender,
  interpolate, spring, staticFile, useCurrentFrame, useVideoConfig, Easing,
} from "remotion";

// ---------------------------------------------------------------------------------------------------
// Fonts: the same faces the demo document uses.
const fontHandle = typeof window !== "undefined" ? delayRender("fonts") : null;
if (typeof window !== "undefined") {
  const faces = [
    new FontFace("Anton", `url(${staticFile("fonts/Anton-Regular.ttf")})`),
    new FontFace("Instrument Serif", `url(${staticFile("fonts/InstrumentSerif-Italic.ttf")})`, { style: "italic" }),
    new FontFace("Space Grotesk", `url(${staticFile("fonts/SpaceGrotesk.ttf")})`, { weight: "300 700" }),
  ];
  Promise.all(faces.map((f) => f.load().then((l) => document.fonts.add(l))))
    .then(() => fontHandle !== null && continueRender(fontHandle))
    .catch(() => fontHandle !== null && continueRender(fontHandle));
}

const DISPLAY = "'Anton', 'Impact', sans-serif";
const SERIF = "'Instrument Serif', Georgia, serif";
const GROTESK = "'Space Grotesk', 'Adwaita Sans', sans-serif";
const MONO = "'JetBrainsMono Nerd Font', 'JetBrains Mono', monospace";

// Palette: the app's navy, the photo's peach and coat orange.
const INK = "#f4efe9";
const MUTED = "#a9b4c2";
const PEACH = "#ffb38a";
const ORANGE = "#e2632c";
const NAVY = "#040a13";

// ---------------------------------------------------------------------------------------------------
// Time: the track's beat grid (117.4 BPM, the kick drops at 15.985 s).
const FPS = 30;
const BEAT = 0.510968;
const DROP = 15.985;
const BAR = BEAT * 4;
const atBar = (n: number) => DROP + n * BAR;
const f = (s: number) => Math.round(s * FPS);
const TOTAL = 87;

const SRC_W = 2560, SRC_H = 1440;
const ease = Easing.bezier(0.22, 1, 0.36, 1);
const clamp = { extrapolateLeft: "clamp", extrapolateRight: "clamp" } as const;

// ---------------------------------------------------------------------------------------------------
// Background: deep navy with two drifting glows that swell on every beat after the drop.
const Background: React.FC = () => {
  const frame = useCurrentFrame();
  const t = frame / FPS;
  let pulse = 0;
  if (t >= DROP && t < 81) {
    const since = ((t - DROP) % BEAT) / BEAT;
    pulse = Math.exp(-since * 5);
  }
  const x1 = 30 + Math.sin(t * 0.21) * 12, y1 = 35 + Math.cos(t * 0.17) * 10;
  const x2 = 72 + Math.cos(t * 0.19) * 10, y2 = 70 + Math.sin(t * 0.23) * 10;
  return (
    <AbsoluteFill style={{ background: NAVY }}>
      <AbsoluteFill style={{ background: `radial-gradient(ellipse 55% 60% at ${x1}% ${y1}%, rgba(255,150,110,${0.16 + pulse * 0.06}) 0%, rgba(255,150,110,0) 70%)` }} />
      <AbsoluteFill style={{ background: `radial-gradient(ellipse 50% 55% at ${x2}% ${y2}%, rgba(110,120,255,${0.12 + pulse * 0.04}) 0%, rgba(110,120,255,0) 70%)` }} />
      <AbsoluteFill style={{ background: "radial-gradient(ellipse 120% 90% at 50% 50%, rgba(0,0,0,0) 55%, rgba(0,0,0,0.55) 100%)" }} />
    </AbsoluteFill>
  );
};

// ---------------------------------------------------------------------------------------------------
// Footage: parts of the captured session laid end to end, each stretched to its share of beats.
type Part = { from: number; to: number; beats: number; tag?: string };
type Cam = { t: number; x: number; y: number; z: number };

const partSeconds = (p: Part) => p.beats * BEAT;

const camAt = (cams: Cam[], t: number): Cam => {
  if (cams.length === 0) return { t, x: SRC_W / 2, y: SRC_H / 2, z: 1 };
  if (t <= cams[0].t) return cams[0];
  for (let i = 0; i < cams.length - 1; i++) {
    const a = cams[i], b = cams[i + 1];
    if (t <= b.t) {
      const k = ease((t - a.t) / Math.max(1e-6, b.t - a.t));
      return { t, x: a.x + (b.x - a.x) * k, y: a.y + (b.y - a.y) * k, z: a.z + (b.z - a.z) * k };
    }
  }
  return cams[cams.length - 1];
};

const Footage: React.FC<{ parts: Part[]; cams: Cam[]; width: number; height: number }> = ({ parts, cams, width, height }) => {
  const frame = useCurrentFrame();
  const t = frame / FPS;
  const fit = width / SRC_W;
  const cam = camAt(cams, t);
  // Keep the focus point centered, without showing past the footage's edges.
  const scale = fit * cam.z;
  let tx = width / 2 - cam.x * scale;
  let ty = height / 2 - cam.y * scale;
  tx = Math.min(0, Math.max(width - SRC_W * scale, tx));
  ty = Math.min(0, Math.max(height - SRC_H * scale, ty));
  let at = 0;
  return (
    <div style={{ position: "absolute", inset: 0, overflow: "hidden", background: NAVY }}>
      <div style={{ position: "absolute", left: 0, top: 0, width: SRC_W, height: SRC_H, transformOrigin: "0 0", transform: `translate(${tx}px, ${ty}px) scale(${scale})` }}>
        {parts.map((p, i) => {
          const start = f(at);
          const dur = Math.max(1, f(at + partSeconds(p)) - start);
          at += partSeconds(p);
          const rate = (p.to - p.from) / partSeconds(p);
          return (
            <Sequence key={i} from={start} durationInFrames={dur} layout="none">
              <OffthreadVideo src={staticFile("session3.mp4")} muted startFrom={f(p.from)} playbackRate={rate}
                style={{ width: SRC_W, height: SRC_H, display: "block" }} />
            </Sequence>
          );
        })}
      </div>
    </div>
  );
};

const SpeedTags: React.FC<{ parts: Part[] }> = ({ parts }) => {
  let at = 0;
  return (
    <>
      {parts.map((p, i) => {
        const start = f(at);
        at += partSeconds(p);
        if (!p.tag) return null;
        return (
          <Sequence key={i} from={start} durationInFrames={f(partSeconds(p))} layout="none">
            <SpeedTag text={p.tag} />
          </Sequence>
        );
      })}
    </>
  );
};

const SpeedTag: React.FC<{ text: string }> = ({ text }) => {
  const frame = useCurrentFrame();
  const { durationInFrames } = useVideoConfig();
  const o = interpolate(frame, [0, 5, durationInFrames - 5, durationInFrames], [0, 1, 1, 0], clamp);
  const spin = (frame * 18) % 360;
  return (
    <div style={{ position: "absolute", right: 60, top: 54, opacity: o, display: "flex", alignItems: "center", gap: 14,
      fontFamily: GROTESK, fontWeight: 600, fontSize: 24, letterSpacing: 2, color: NAVY, background: PEACH,
      padding: "10px 20px", borderRadius: 999, textTransform: "uppercase", boxShadow: "0 10px 30px rgba(0,0,0,.4)" }}>
      <div style={{ width: 18, height: 18, borderRadius: 9, border: `3px solid ${NAVY}`, borderTopColor: "transparent", transform: `rotate(${spin}deg)` }} />
      {text}
    </div>
  );
};

// ---------------------------------------------------------------------------------------------------
// Captions: an eyebrow, a headline that rises word by word, a serif line.
type Cap = { eyebrow: string; title: string; sub?: string };

const Rise: React.FC<{ children: React.ReactNode; delay: number; out: number; style?: React.CSSProperties }> = ({ children, delay, out, style }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  const s = spring({ frame: frame - delay, fps, config: { damping: 20, stiffness: 170, mass: 0.7 } });
  const o = interpolate(frame, [out, out + 7], [0, 1], clamp);
  const y = (1 - s) * 105 - o * 105;
  return (
    <span style={{ display: "inline-block", overflow: "hidden", verticalAlign: "bottom", paddingBottom: "0.06em", ...style }}>
      <span style={{ display: "inline-block", transform: `translateY(${y}%)` }}>{children}</span>
    </span>
  );
};

const Caption: React.FC<{ cap: Cap; x: number; y: number; width: number; align?: "left" | "center"; size?: number; scrim?: boolean; top?: boolean }> = ({ cap, x, y, width, align = "left", size = 104, scrim, top }) => {
  const frame = useCurrentFrame();
  const { durationInFrames } = useVideoConfig();
  const out = durationInFrames - 9;
  const words = cap.title.split(" ");
  const lineIn = interpolate(frame, [2, 14], [0, 1], { ...clamp, easing: ease });
  const lineOut = interpolate(frame, [out, out + 8], [1, 0], clamp);
  return (
    <>
      {scrim ? <AbsoluteFill style={{ background: `linear-gradient(${top ? 180 : 0}deg, rgba(4,10,19,0.92) 0%, rgba(4,10,19,0.55) 36%, rgba(4,10,19,0) 62%)`, opacity: lineOut }} /> : null}
      <div style={{ position: "absolute", left: x, top: y, width, textAlign: align }}>
        <div style={{ display: "flex", alignItems: "center", gap: 14, justifyContent: align === "center" ? "center" : "flex-start", marginBottom: 18 }}>
          <div style={{ width: 46 * lineIn * lineOut, height: 3, background: PEACH }} />
          <Rise delay={3} out={out}><span style={{ fontFamily: GROTESK, fontWeight: 600, fontSize: 24, letterSpacing: 5, textTransform: "uppercase", color: PEACH }}>{cap.eyebrow}</span></Rise>
        </div>
        <div style={{ fontFamily: DISPLAY, fontSize: size, lineHeight: 0.98, color: INK, textTransform: "uppercase", letterSpacing: 0.5 }}>
          {words.map((w, i) => (
            <React.Fragment key={i}>
              <Rise delay={5 + i * 2} out={out}>{w}</Rise>{i < words.length - 1 ? " " : ""}
            </React.Fragment>
          ))}
        </div>
        {cap.sub ? (
          <div style={{ marginTop: 18, fontFamily: SERIF, fontStyle: "italic", fontSize: 40, lineHeight: 1.15, color: MUTED }}>
            <Rise delay={12} out={out}>{cap.sub}</Rise>
          </div>
        ) : null}
      </div>
    </>
  );
};

// ---------------------------------------------------------------------------------------------------
// Scenes over footage: a full-frame camera, or the app as a tilted window beside the caption.
type Scene = { key: string; bar: number; bars: number; parts: Part[]; cams?: Cam[]; layout: "full" | "tilt"; cap: Cap; chips?: string[]; capTop?: boolean };

const Punch: React.FC<{ children: React.ReactNode }> = ({ children }) => {
  const frame = useCurrentFrame();
  const s = interpolate(frame, [0, 9], [1.07, 1], { ...clamp, easing: ease });
  const blur = interpolate(frame, [0, 6], [14, 0], clamp);
  const x = interpolate(frame, [0, 8], [90, 0], { ...clamp, easing: ease });
  return <AbsoluteFill style={{ transform: `translateX(${x}px) scale(${s})`, filter: `blur(${blur}px)` }}>{children}</AbsoluteFill>;
};

const Chips: React.FC<{ chips: string[] }> = ({ chips }) => {
  const frame = useCurrentFrame();
  const { fps, durationInFrames } = useVideoConfig();
  const out = interpolate(frame, [durationInFrames - 8, durationInFrames - 1], [1, 0], clamp);
  return (
    <div style={{ position: "absolute", right: 70, bottom: 70, display: "flex", gap: 16, opacity: out }}>
      {chips.map((c, i) => {
        const s = spring({ frame: frame - f(BEAT * (2 + i)), fps, config: { damping: 12, stiffness: 180 } });
        return (
          <div key={c} style={{ transform: `scale(${s})`, fontFamily: GROTESK, fontWeight: 600, fontSize: 26, color: INK,
            background: "rgba(10,20,34,0.78)", border: "1px solid rgba(255,179,138,0.55)", padding: "12px 24px", borderRadius: 999,
            backdropFilter: "blur(10px)", boxShadow: "0 10px 30px rgba(0,0,0,.35)" }}>{c}</div>
        );
      })}
    </div>
  );
};

const FootageScene: React.FC<{ s: Scene }> = ({ s }) => {
  const frame = useCurrentFrame();
  const { durationInFrames } = useVideoConfig();
  const t = frame / FPS;
  if (s.layout === "full") {
    return (
      <AbsoluteFill>
        <Punch><Footage parts={s.parts} cams={s.cams ?? []} width={1920} height={1080} /></Punch>
        <SpeedTags parts={s.parts} />
        <Caption cap={s.cap} x={90} y={s.capTop ? 90 : 640} width={1100} scrim top={s.capTop} />
        {s.chips ? <Chips chips={s.chips} /> : null}
      </AbsoluteFill>
    );
  }
  // Tilt: the window turns slowly toward the viewer while the scene plays.
  const W = 1240, H = W * SRC_H / SRC_W;
  const k = t / (durationInFrames / FPS);
  const ry = -20 + 9 * k, rx = 6 - 3 * k;
  const enter = interpolate(frame, [0, 12], [0, 1], { ...clamp, easing: ease });
  return (
    <AbsoluteFill>
      <div style={{ position: "absolute", inset: 0, perspective: 2200 }}>
        <div style={{ position: "absolute", left: 610, top: (1080 - H) / 2, width: W, height: H,
          transform: `translateX(${(1 - enter) * 260}px) rotateY(${ry}deg) rotateX(${rx}deg) scale(${0.94 + 0.04 * k})`,
          transformStyle: "preserve-3d", borderRadius: 16, overflow: "hidden", opacity: enter,
          boxShadow: "0 60px 120px rgba(0,0,0,0.65), 0 0 0 1px rgba(255,255,255,0.08)" }}>
          <Footage parts={s.parts} cams={s.cams ?? []} width={W} height={H} />
        </div>
      </div>
      <SpeedTags parts={s.parts} />
      <Caption cap={s.cap} x={90} y={330} width={560} size={92} />
      {s.chips ? <Chips chips={s.chips} /> : null}
    </AbsoluteFill>
  );
};

// Focus points are in the 2560 x 1440 footage.
const SCENES: Scene[] = [
  { key: "curves", bar: 0, bars: 2, layout: "full",
    parts: [{ from: 6.5, to: 10.2, beats: 8 }],
    cams: [{ t: 0, x: 1280, y: 720, z: 1.05 }, { t: 1.0, x: 960, y: 560, z: 1.55 }, { t: 4.1, x: 980, y: 560, z: 1.7 }],
    cap: { eyebrow: "Adjustments", title: "Curves, Levels and Hue adjustments.", sub: "Live previews on the real dialogs." } },
  { key: "hsl", bar: 2, bars: 1, layout: "tilt",
    parts: [{ from: 10.5, to: 13.2, beats: 4 }],
    cap: { eyebrow: "Adjustments", title: "Every one a layer.", sub: "Non-destructive, stack them freely." } },
  { key: "removebg", bar: 3, bars: 2, layout: "full",
    parts: [{ from: 14.5, to: 15.9, beats: 2 }, { from: 16.2, to: 18.4, beats: 6 }],
    cams: [{ t: 0, x: 1100, y: 700, z: 1.15 }, { t: 4, x: 1100, y: 650, z: 1.45 }],
    cap: { eyebrow: "On-device AI", title: "Cut out the subject.", sub: "Background removal runs locally. Nothing leaves your machine." } },
  { key: "type", bar: 5, bars: 2, layout: "full",
    parts: [{ from: 18.95, to: 22.1, beats: 8 }],
    cams: [{ t: 0, x: 820, y: 520, z: 1.25 }, { t: 4.1, x: 820, y: 470, z: 1.85 }],
    cap: { eyebrow: "Type", title: "Full type controls.", sub: "Any font on your system. Editable forever." } },
  { key: "style", bar: 7, bars: 1, layout: "tilt",
    parts: [{ from: 22.5, to: 25.7, beats: 4 }],
    cap: { eyebrow: "Layer styles", title: "Style your layers.", sub: "Shadows, glows, bevels, strokes, overlays." } },
  { key: "type2", bar: 8, bars: 1, layout: "full",
    parts: [{ from: 26.0, to: 28.8, beats: 4 }],
    cams: [{ t: 0, x: 820, y: 1010, z: 1.35 }, { t: 2.0, x: 820, y: 990, z: 1.75 }],
    cap: { eyebrow: "Type", title: "Layout your design.", sub: "Point and paragraph type, real alignment." }, capTop: true },
  { key: "genfill", bar: 9, bars: 2, layout: "full",
    parts: [{ from: 29.2, to: 32.7, beats: 3 }, { from: 65.3, to: 67.4, beats: 5, tag: "fill landed" }],
    cams: [{ t: 0, x: 900, y: 700, z: 1.25 }, { t: 1.5, x: 700, y: 820, z: 1.55 }, { t: 4.1, x: 720, y: 860, z: 1.9 }],
    cap: { eyebrow: "Generative Fill", title: "Adjustments with a prompt.", sub: "“Two flamingos, far off.” Matched to the light." },
    chips: ["fal.ai, your key", "or local ComfyUI"] },
  { key: "expand", bar: 11, bars: 2, layout: "full",
    parts: [{ from: 68.4, to: 70.8, beats: 3 }, { from: 80.7, to: 83.2, beats: 5 }],
    cams: [{ t: 0, x: 1100, y: 700, z: 1.35 }, { t: 1.5, x: 1100, y: 720, z: 1.05 }, { t: 4.1, x: 1100, y: 720, z: 1.12 }],
    cap: { eyebrow: "Generative Expand", title: "Portrait to panorama.", sub: "One seamless photo, grown past its edges." } },
  { key: "assistant", bar: 13, bars: 5, layout: "full",
    parts: [{ from: 84.2, to: 86.8, beats: 5 }, { from: 86.8, to: 151.5, beats: 10, tag: "13× · Compy is working" }, { from: 151.5, to: 155.3, beats: 5 }],
    cams: [{ t: 0, x: 2150, y: 900, z: 1.75 }, { t: 2.4, x: 2150, y: 900, z: 1.75 }, { t: 3.3, x: 1280, y: 720, z: 1.0 }, { t: 7.6, x: 1280, y: 720, z: 1.0 }, { t: 10.2, x: 2100, y: 1100, z: 1.45 }],
    cap: { eyebrow: "The Compy agent", title: "Just ask.", sub: "Runs on Claude Code, the agent you already use with Omarchy." } },
];

// ---------------------------------------------------------------------------------------------------
// The prompt, typed big over the assistant scene.
const PromptBubble: React.FC = () => {
  const frame = useCurrentFrame();
  const { durationInFrames } = useVideoConfig();
  const text = "Give this a warm golden-hour grade, add a soft glow where the sun sits on the horizon, and finish with a subtle film grain.";
  const n = Math.floor(interpolate(frame, [6, 60], [0, text.length], clamp));
  const o = interpolate(frame, [0, 6, durationInFrames - 8, durationInFrames], [0, 1, 1, 0], clamp);
  const y = interpolate(frame, [0, 10], [30, 0], { ...clamp, easing: ease });
  return (
    <div style={{ position: "absolute", left: 90, top: 110, width: 900, opacity: o, transform: `translateY(${y}px)`,
      background: "rgba(10,20,34,0.86)", border: "1px solid rgba(255,255,255,0.1)", borderRadius: 22, padding: "28px 34px",
      boxShadow: "0 30px 80px rgba(0,0,0,0.5)", backdropFilter: "blur(12px)" }}>
      <div style={{ fontFamily: GROTESK, fontWeight: 600, fontSize: 20, letterSpacing: 4, color: PEACH, textTransform: "uppercase", marginBottom: 12 }}>You</div>
      <div style={{ fontFamily: GROTESK, fontSize: 36, lineHeight: 1.3, color: INK }}>
        {text.slice(0, n)}<span style={{ opacity: frame % 16 < 8 ? 1 : 0, color: PEACH }}>▍</span>
      </div>
    </div>
  );
};

// ---------------------------------------------------------------------------------------------------
// Before and after the agent's grade: a wipe across the picture.
const BeforeAfter: React.FC = () => {
  const frame = useCurrentFrame();
  const { durationInFrames } = useVideoConfig();
  const W = 1300, H = W * 1090 / 1516;
  const p = interpolate(frame, [8, durationInFrames - 22], [0.02, 0.98], { ...clamp, easing: Easing.inOut(Easing.cubic) });
  const enter = interpolate(frame, [0, 12], [0, 1], { ...clamp, easing: ease });
  const zoom = 1 + frame / durationInFrames * 0.05;
  return (
    <AbsoluteFill>
      <div style={{ position: "absolute", left: 540, top: (1080 - H) / 2, width: W, height: H, borderRadius: 14, overflow: "hidden",
        opacity: enter, transform: `scale(${(0.95 + 0.05 * enter) * zoom})`, boxShadow: "0 50px 110px rgba(0,0,0,0.6)" }}>
        <Img src={staticFile("after.png")} style={{ position: "absolute", width: W, height: H }} />
        <div style={{ position: "absolute", inset: 0, clipPath: `inset(0 0 0 ${p * 100}%)` }}>
          <Img src={staticFile("before.png")} style={{ position: "absolute", width: W, height: H }} />
        </div>
        <div style={{ position: "absolute", top: 0, bottom: 0, left: `${p * 100}%`, width: 4, marginLeft: -2, background: INK, boxShadow: "0 0 30px rgba(255,255,255,0.8)" }} />
        <div style={{ position: "absolute", left: 24, bottom: 20, fontFamily: GROTESK, fontWeight: 600, fontSize: 22, letterSpacing: 4, color: INK, opacity: p > 0.12 ? 1 : 0 }}>AFTER</div>
        <div style={{ position: "absolute", right: 24, bottom: 20, fontFamily: GROTESK, fontWeight: 600, fontSize: 22, letterSpacing: 4, color: INK, opacity: p < 0.88 ? 1 : 0 }}>BEFORE</div>
      </div>
      <Caption cap={{ eyebrow: "The Compy agent", title: "Graded. Glowing. Grained.", sub: "Three new layers, all still editable." }} x={90} y={330} width={430} size={84} />
    </AbsoluteFill>
  );
};

// ---------------------------------------------------------------------------------------------------
// Export: the dialog and artboards (a tilt scene) then the three files themselves.
const EXPORT: Scene = { key: "export", bar: 20, bars: 2, layout: "tilt",
  parts: [{ from: 157.4, to: 160.4, beats: 4 }, { from: 166.3, to: 168.8, beats: 4 }],
  cap: { eyebrow: "Export Sizes", title: "Every size at once.", sub: "Instagram, Story and X, reframed as artboards." } };

const Showcase: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps, durationInFrames } = useVideoConfig();
  const cards = [
    { src: "out_story.png", w: 1080, h: 1920, x: 250, y: 120, h2: 840, r: -7, d: 0 },
    { src: "out_ig.png", w: 1080, h: 1080, x: 720, y: 250, h2: 600, r: 3, d: 4 },
    { src: "out_x.png", w: 1600, h: 900, x: 1060, y: 330, h2: 480, r: -2, d: 8 },
  ];
  const out = interpolate(frame, [durationInFrames - 8, durationInFrames], [1, 0], clamp);
  return (
    <AbsoluteFill style={{ opacity: out }}>
      <div style={{ position: "absolute", inset: 0, perspective: 1800 }}>
        {cards.map((c, i) => {
          const s = spring({ frame: frame - c.d, fps, config: { damping: 16, stiffness: 90 } });
          const float = Math.sin((frame + i * 20) / 22) * 8;
          const hw = c.h2 * c.w / c.h;
          return (
            <div key={c.src} style={{ position: "absolute", left: c.x, top: c.y + float + (1 - s) * 500, width: hw, height: c.h2,
              transform: `rotate(${c.r * s}deg) rotateY(${(1 - s) * 40}deg)`, opacity: s, borderRadius: 12, overflow: "hidden",
              boxShadow: "0 40px 90px rgba(0,0,0,0.6)" }}>
              <Img src={staticFile(c.src)} style={{ width: hw, height: c.h2 }} />
            </div>
          );
        })}
      </div>
      <div style={{ position: "absolute", left: 0, right: 0, bottom: 60, textAlign: "center" }}>
        <div style={{ fontFamily: SERIF, fontStyle: "italic", fontSize: 54, color: INK, textShadow: "0 6px 30px rgba(0,0,0,.6)" }}>
          <Rise delay={10} out={durationInFrames}>Three files, one click, no bloat.</Rise>
        </div>
      </div>
    </AbsoluteFill>
  );
};

// ---------------------------------------------------------------------------------------------------
// Themes: the same document under six Omarchy themes, a cut every two beats.
const THEMES = [
  ["theme_catppuccin.png", "Catppuccin"], ["theme_rose-pine.png", "Rosé Pine"], ["theme_gruvbox.png", "Gruvbox"],
  ["theme_everforest.png", "Everforest"], ["theme_tokyo-night.png", "Tokyo Night"], ["theme_white.png", "White"],
];

const Themes: React.FC = () => {
  const frame = useCurrentFrame();
  const each = f(BEAT * 2);
  const idx = Math.min(THEMES.length - 1, Math.floor(frame / each));
  const local = frame - idx * each;
  const punch = interpolate(local, [0, 8], [1.06, 1], { ...clamp, easing: ease });
  const W = 1500, H = W * 1440 / 2560;
  return (
    <AbsoluteFill>
      <div style={{ position: "absolute", left: (1920 - W) / 2, top: 70, width: W, height: H, borderRadius: 14, overflow: "hidden",
        transform: `scale(${punch})`, boxShadow: "0 50px 110px rgba(0,0,0,0.6)" }}>
        <Img src={staticFile(THEMES[idx][0])} style={{ width: W, height: H }} />
      </div>
      <div style={{ position: "absolute", left: 0, right: 0, top: 70 + H + 44, display: "flex", justifyContent: "center", alignItems: "baseline", gap: 26 }}>
        <div style={{ fontFamily: DISPLAY, fontSize: 64, color: INK, textTransform: "uppercase" }}>Follows your Omarchy theme</div>
        <div style={{ fontFamily: SERIF, fontStyle: "italic", fontSize: 50, color: PEACH, minWidth: 260 }}>{THEMES[idx][1]}</div>
      </div>
    </AbsoluteFill>
  );
};

// ---------------------------------------------------------------------------------------------------
// The feature wall: rows of real tool names sliding past, one lit on every beat.
const ROWS = [
  ["Move", "Marquee", "Lasso", "Magic Wand", "Crop", "Brush", "Eraser", "Spot Healing", "Clone Stamp", "Liquify", "Dodge", "Burn", "Gradient", "Pen"],
  ["Layer Masks", "Clipping Masks", "Blend Modes", "Artboards", "Free Transform", "Groups", "Smudge", "Sponge", "Pattern Stamp", "Shapes"],
  ["Levels", "Curves", "Color Balance", "Selective Color", "Channel Mixer", "Gradient Map", "Shadows/Highlights", "Photo Filter", "Grain"],
  ["PSD In & Out", "ABR Brushes", "Content-Aware Fill", "Smart Sharpen", "High Pass", "Lens Correction", "HEIC", "AVIF", "Export Layers"],
];

const Wall: React.FC = () => {
  const frame = useCurrentFrame();
  const { durationInFrames } = useVideoConfig();
  const beat = Math.floor(frame / (BEAT * FPS));
  const o = interpolate(frame, [0, 6, durationInFrames - 8, durationInFrames], [0, 1, 1, 0], clamp);
  return (
    <AbsoluteFill style={{ opacity: o }}>
      <div style={{ position: "absolute", inset: 0, transform: "rotate(-6deg) scale(1.15)", display: "flex", flexDirection: "column", justifyContent: "center", gap: 18 }}>
        {ROWS.map((row, r) => {
          const dir = r % 2 === 0 ? -1 : 1;
          const x = dir * frame * (5 + r) - (dir > 0 ? 1600 : 0);
          const items = [...row, ...row, ...row];
          return (
            <div key={r} style={{ whiteSpace: "nowrap", transform: `translateX(${x}px)` }}>
              {items.map((w, i) => {
                const lit = (i + r * 3) % 7 === beat % 7;
                return (
                  <span key={i} style={{ fontFamily: DISPLAY, fontSize: 110, textTransform: "uppercase", marginRight: 60,
                    color: lit ? ORANGE : "transparent", WebkitTextStroke: lit ? "0" : "2px rgba(244,239,233,0.28)" }}>{w}</span>
                );
              })}
            </div>
          );
        })}
      </div>
      <AbsoluteFill style={{ background: "radial-gradient(ellipse 45% 40% at 50% 50%, rgba(4,10,19,0.92) 30%, rgba(4,10,19,0) 100%)" }} />
      <AbsoluteFill style={{ justifyContent: "center", alignItems: "center", textAlign: "center" }}>
        <div style={{ fontFamily: DISPLAY, fontSize: 150, lineHeight: 0.95, color: INK, textTransform: "uppercase" }}>
          <Rise delay={2} out={durationInFrames}>All the tools.</Rise><br />
          <Rise delay={f(BEAT * 2)} out={durationInFrames}><span style={{ color: PEACH }}>None of the bloat.</span></Rise>
        </div>
      </AbsoluteFill>
    </AbsoluteFill>
  );
};

// ---------------------------------------------------------------------------------------------------
// Speaks Photoshop: files, brushes and the keys under your fingers.
const KEYS = ["V", "M", "B", "T", "Ctrl J", "Ctrl T", "Ctrl M", "Ctrl L", "Ctrl U", "Ctrl E"];

const SpeaksPhotoshop: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps, durationInFrames } = useVideoConfig();
  const beat = Math.floor(frame / (BEAT * FPS));
  const out = interpolate(frame, [durationInFrames - 8, durationInFrames], [1, 0], clamp);
  const cards = [
    { k: ".PSD", v: "Open and save layered Photoshop files." },
    { k: ".ABR", v: "Your Photoshop brush packs load as they are." },
  ];
  return (
    <AbsoluteFill style={{ justifyContent: "center", alignItems: "center", opacity: out }}>
      <div style={{ fontFamily: DISPLAY, fontSize: 120, color: INK, textTransform: "uppercase", lineHeight: 1, marginBottom: 46 }}>
        <Rise delay={0} out={durationInFrames}>Speaks</Rise> <Rise delay={3} out={durationInFrames}><span style={{ color: PEACH }}>Photoshop.</span></Rise>
      </div>
      <div style={{ display: "flex", gap: 30, alignItems: "stretch" }}>
        {cards.map((c, i) => {
          const sp = spring({ frame: frame - f(BEAT * (1 + i)), fps, config: { damping: 15, stiffness: 140 } });
          return (
            <div key={c.k} style={{ width: 430, padding: "34px 38px", borderRadius: 22, background: "rgba(12,22,38,0.85)",
              border: "1px solid rgba(255,255,255,0.09)", transform: `translateY(${(1 - sp) * 80}px)`, opacity: sp, boxShadow: "0 30px 80px rgba(0,0,0,0.45)" }}>
              <div style={{ fontFamily: DISPLAY, fontSize: 84, color: ORANGE, lineHeight: 1 }}>{c.k}</div>
              <div style={{ marginTop: 14, fontFamily: SERIF, fontStyle: "italic", fontSize: 36, color: MUTED, lineHeight: 1.2 }}>{c.v}</div>
            </div>
          );
        })}
        {(() => {
          const sp = spring({ frame: frame - f(BEAT * 3), fps, config: { damping: 15, stiffness: 140 } });
          return (
            <div style={{ width: 560, padding: "34px 38px", borderRadius: 22, background: "rgba(12,22,38,0.85)",
              border: "1px solid rgba(255,255,255,0.09)", transform: `translateY(${(1 - sp) * 80}px)`, opacity: sp, boxShadow: "0 30px 80px rgba(0,0,0,0.45)" }}>
              <div style={{ fontFamily: DISPLAY, fontSize: 56, color: INK, textTransform: "uppercase", lineHeight: 1 }}>The same shortcuts</div>
              <div style={{ marginTop: 22, display: "flex", flexWrap: "wrap", gap: 12 }}>
                {KEYS.map((k, i) => {
                  const lit = i === beat % KEYS.length;
                  return (
                    <div key={k} style={{ fontFamily: GROTESK, fontWeight: 600, fontSize: 28, padding: "10px 16px", borderRadius: 10,
                      color: lit ? NAVY : INK, background: lit ? PEACH : "rgba(255,255,255,0.06)",
                      border: "1px solid rgba(255,255,255,0.16)", borderBottomWidth: 4, transform: `translateY(${lit ? 3 : 0}px)` }}>{k}</div>
                  );
                })}
              </div>
            </div>
          );
        })()}
      </div>
    </AbsoluteFill>
  );
};

// ---------------------------------------------------------------------------------------------------
// Where the AI runs: three cards, one per beat pair.
const AICards: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps, durationInFrames } = useVideoConfig();
  const cards = [
    { k: "Generative Fill & Expand", v: "fal.ai with your own key, or a local ComfyUI." },
    { k: "Background removal", v: "An ONNX model on your machine. Free and offline." },
    { k: "The Compy agent", v: "Claude Code, the agent you use with Omarchy." },
  ];
  const out = interpolate(frame, [durationInFrames - 8, durationInFrames], [1, 0], clamp);
  return (
    <AbsoluteFill style={{ justifyContent: "center", alignItems: "center", opacity: out }}>
      <div style={{ fontFamily: GROTESK, fontWeight: 600, fontSize: 26, letterSpacing: 6, color: PEACH, textTransform: "uppercase", marginBottom: 40 }}>
        <Rise delay={0} out={durationInFrames}>AI where you want it</Rise>
      </div>
      <div style={{ display: "flex", gap: 34 }}>
        {cards.map((c, i) => {
          const s = spring({ frame: frame - f(BEAT * (1 + i)), fps, config: { damping: 15, stiffness: 140 } });
          return (
            <div key={c.k} style={{ width: 520, padding: "38px 40px", borderRadius: 22, background: "rgba(12,22,38,0.85)",
              border: "1px solid rgba(255,255,255,0.09)", transform: `translateY(${(1 - s) * 80}px)`, opacity: s,
              boxShadow: "0 30px 80px rgba(0,0,0,0.45)" }}>
              <div style={{ fontFamily: DISPLAY, fontSize: 50, color: INK, textTransform: "uppercase", lineHeight: 1 }}>{c.k}</div>
              <div style={{ marginTop: 18, fontFamily: SERIF, fontStyle: "italic", fontSize: 36, color: MUTED, lineHeight: 1.2 }}>{c.v}</div>
            </div>
          );
        })}
      </div>
    </AbsoluteFill>
  );
};

// ---------------------------------------------------------------------------------------------------
// Intro and outro.
const Letters: React.FC<{ text: string; start: number; step: number; size: number; color?: string }> = ({ text, start, step, size, color = INK }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  return (
    <div style={{ fontFamily: DISPLAY, fontSize: size, color, textTransform: "uppercase", lineHeight: 1, whiteSpace: "nowrap" }}>
      {text.split("").map((ch, i) => {
        const s = spring({ frame: frame - start - i * step, fps, config: { damping: 14, stiffness: 160 } });
        return <span key={i} style={{ display: "inline-block", transform: `translateY(${(1 - s) * 60}px) scale(${0.6 + 0.4 * s})`, opacity: s, whiteSpace: "pre" }}>{ch}</span>;
      })}
    </div>
  );
};

const Intro: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  const t = frame / FPS;
  // 0 to 8 s: the promise, in two lines. 8 to 13.4: the app floats in with the name. 13.4 to 16: flash cuts.
  const phase1Out = interpolate(t, [7.4, 8.0], [1, 0], clamp);
  const W = 1320, H = W * SRC_H / SRC_W;
  const winIn = spring({ frame: frame - f(8.0), fps, config: { damping: 18, stiffness: 60 } });
  const ry = interpolate(t, [8, 13.4], [24, -8], clamp);
  const logo = spring({ frame: frame - f(10.3), fps, config: { damping: 13, stiffness: 120 } });
  const flashes = ["out_x.png", "theme_white.png", "after.png", "out_story.png", "theme_tokyo-night.png"];
  const fl = Math.floor((t - 13.43) / BEAT);
  return (
    <AbsoluteFill>
      {t < 8.05 ? (
        <AbsoluteFill style={{ justifyContent: "center", alignItems: "center", opacity: phase1Out }}>
          <Letters text="All of Photoshop's tools." start={f(0.5)} step={2} size={124} />
          <div style={{ height: 24 }} />
          <Letters text="None of the bloat." start={f(4.0)} step={2} size={124} color={PEACH} />
        </AbsoluteFill>
      ) : null}
      {t >= 8 && t < 13.43 ? (
        <AbsoluteFill>
          <div style={{ position: "absolute", inset: 0, perspective: 2400 }}>
            <div style={{ position: "absolute", left: (1920 - W) / 2, top: 40 + (1 - winIn) * 400, width: W, height: H,
              transform: `rotateY(${ry}deg) rotateX(10deg) scale(${0.8 + 0.1 * winIn})`, opacity: winIn, borderRadius: 16, overflow: "hidden",
              boxShadow: "0 80px 140px rgba(0,0,0,0.7), 0 0 0 1px rgba(255,255,255,0.08)" }}>
              <Sequence from={f(8.0)} layout="none"><Footage parts={[{ from: 161.0, to: 170.0, beats: 11 }]} cams={[]} width={W} height={H} /></Sequence>
            </div>
          </div>
          <AbsoluteFill style={{ background: "linear-gradient(0deg, rgba(4,10,19,0.95) 0%, rgba(4,10,19,0.2) 45%, rgba(4,10,19,0) 60%)" }} />
          <div style={{ position: "absolute", left: 0, right: 0, bottom: 60, display: "flex", flexDirection: "column", alignItems: "center", opacity: logo, transform: `translateY(${(1 - logo) * 40}px)` }}>
            <div style={{ display: "flex", alignItems: "center", gap: 30 }}>
              <Img src={staticFile("mark-glyph.png")} style={{ height: 130 }} />
              <div style={{ fontFamily: DISPLAY, fontSize: 190, color: INK, lineHeight: 0.9, textTransform: "uppercase" }}>Compy</div>
            </div>
            <div style={{ marginTop: 14, fontFamily: SERIF, fontStyle: "italic", fontSize: 50, color: PEACH }}>
              <Rise delay={f(12.3)} out={9999}>the photo editor for Omarchy</Rise>
            </div>
          </div>
        </AbsoluteFill>
      ) : null}
      {t >= 13.43 ? (
        <AbsoluteFill style={{ background: NAVY }}>
          <Img src={staticFile(flashes[Math.max(0, Math.min(flashes.length - 1, fl))])}
            style={{ width: "100%", height: "100%", objectFit: "cover", transform: `scale(${1.12 - ((t - 13.43) % BEAT) / BEAT * 0.1})` }} />
          <AbsoluteFill style={{ background: `rgba(255,255,255,${Math.max(0, 0.5 - ((t - 13.43) % BEAT) / BEAT * 2)})` }} />
        </AbsoluteFill>
      ) : null}
    </AbsoluteFill>
  );
};

const Outro: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps, durationInFrames } = useVideoConfig();
  const s = spring({ frame, fps, config: { damping: 14, stiffness: 90 } });
  const cmd = spring({ frame: frame - 14, fps, config: { damping: 16, stiffness: 120 } });
  const end = interpolate(frame, [durationInFrames - 20, durationInFrames], [1, 0], clamp);
  return (
    <AbsoluteFill style={{ justifyContent: "center", alignItems: "center", opacity: end }}>
      <div style={{ display: "flex", alignItems: "center", gap: 30, opacity: s, transform: `scale(${0.9 + 0.1 * s})` }}>
        <Img src={staticFile("mark-glyph.png")} style={{ height: 150 }} />
        <div style={{ fontFamily: DISPLAY, fontSize: 220, color: INK, lineHeight: 0.9, textTransform: "uppercase" }}>Compy</div>
      </div>
      <div style={{ marginTop: 22, fontFamily: SERIF, fontStyle: "italic", fontSize: 50, color: PEACH, opacity: s }}>
        Free, open source, built for Omarchy.
      </div>
      <div style={{ marginTop: 48, opacity: cmd, transform: `translateY(${(1 - cmd) * 24}px)`, fontFamily: MONO, fontSize: 30, color: INK,
        background: "rgba(255,255,255,0.05)", border: "1px solid rgba(255,255,255,0.12)", borderLeft: `4px solid ${PEACH}`, padding: "22px 34px", borderRadius: 10 }}>
        curl -fsSL https://raw.githubusercontent.com/fluxcapctr/compy/main/get.sh | bash
      </div>
      <div style={{ marginTop: 30, opacity: cmd, fontFamily: GROTESK, fontWeight: 600, fontSize: 32, color: MUTED, letterSpacing: 2 }}>
        github.com/fluxcapctr/compy
      </div>
    </AbsoluteFill>
  );
};

// ---------------------------------------------------------------------------------------------------
const Launch: React.FC = () => {
  const frame = useCurrentFrame();
  const vol = (fr: number) => interpolate(fr, [0, 12, f(81.5), f(86.5)], [0, 1, 1, 0], clamp);
  const seq = (from: number, to: number) => ({ from: f(from), durationInFrames: f(to) - f(from) });
  return (
    <AbsoluteFill style={{ background: NAVY }}>
      <Audio src={staticFile("track.mp3")} volume={vol} />
      <Background />
      <Sequence {...seq(0, DROP)}><Intro /></Sequence>
      {SCENES.map((s) => (
        <Sequence key={s.key} {...seq(atBar(s.bar), atBar(s.bar + s.bars))} premountFor={30}><FootageScene s={s} /></Sequence>
      ))}
      <Sequence {...seq(atBar(13), atBar(13) + BEAT * 5)}><PromptBubble /></Sequence>
      <Sequence {...seq(atBar(18), atBar(20))}><BeforeAfter /></Sequence>
      <Sequence {...seq(atBar(20), atBar(22))} premountFor={30}><FootageScene s={EXPORT} /></Sequence>
      <Sequence {...seq(atBar(22), 64.01)}><Showcase /></Sequence>
      <Sequence {...seq(64.01, 64.01 + BAR * 3)}><Themes /></Sequence>
      <Sequence {...seq(64.01 + BAR * 3, 64.01 + BAR * 5)}><SpeaksPhotoshop /></Sequence>
      <Sequence {...seq(64.01 + BAR * 5, 64.01 + BAR * 7)}><Wall /></Sequence>
      <Sequence {...seq(64.01 + BAR * 7, 64.01 + BAR * 9)}><AICards /></Sequence>
      <Sequence {...seq(64.01 + BAR * 9, TOTAL)}><Outro /></Sequence>
      {frame < 0 ? null : null}
    </AbsoluteFill>
  );
};

export const Root: React.FC = () => (
  <Composition id="Launch" component={Launch} width={1920} height={1080} fps={FPS} durationInFrames={f(TOTAL)} />
);
