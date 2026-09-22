# Launch video

A [Remotion](https://www.remotion.dev) composition that cuts the captured editor session into the launch
video attached to the release.

The footage is not in the repo. Put these in `public/` before rendering:

- `session.mp4`: the scripted editor session (2350x1400, 60 fps), captured with gpu-screen-recorder
- `theme_catppuccin.png`, `theme_gruvbox.png`, `theme_kanagawa.png`: stills of the same document under other
  Omarchy themes, taken with `COMPOSITOR_THEME_COLORS`
- `mark-glyph.png`: the mark cropped out of `assets/compy-logo.png`

Scene cut points in `src/Root.tsx` are seconds into `session.mp4`; a new capture needs new marks.

```
npm install
npm run studio    # preview
npm run render    # out/compy-launch.mp4
```
