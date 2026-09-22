# Launch video

A [Remotion](https://www.remotion.dev) composition that cuts the captured editor session into the launch
video attached to the release.

The footage, fonts and music are not in the repo. Put these in `public/` before rendering:

- `session3.mp4`: the scripted editor session, 2560x1440 at 30 fps, grabbed from a nested headless
  Hyprland display so nothing else on the desktop is recorded
- `track.mp3`: the music (117.4 BPM; cuts land on its beat grid, set at the top of `src/Root.tsx`)
- `fonts/`: Anton, Instrument Serif Italic and Space Grotesk from Google Fonts
- `theme_*.png`: the finished document under six Omarchy themes, taken with `COMPOSITOR_THEME_COLORS`
- `before.png`, `after.png`, `out_*.png`: the canvas before and after the assistant's grade, and the
  three exported sizes
- `mark-glyph.png`: the mark cropped out of `assets/compy-logo.png`

Scene cut points in `src/Root.tsx` are seconds into `session3.mp4`; a new capture needs new marks.

```
npm install
npm run studio    # preview
npm run render    # out/compy-launch.mp4
```
