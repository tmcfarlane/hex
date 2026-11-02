# Hex buddy rehearsal

The native character follows the homepage's calm head shader. Word flight uses
Hex's existing onboarding renderer and OpenCode theme. The component uses Rune;
the scripted scene uses a real `rune-kit::TextInput` as its destination.

On macOS, rehearse the production character and word renderer without starting
a listener, microphone, Accessibility query, or settings store:

```sh
hex app -- --scene hex-buddy --video hex-buddy.mp4 --duration 4 --fps 60 --scale 2
hex app -- --scene hex-buddy --snap speaking.png --at 0.35 --theme dark --scale 2
hex app -- --scene hex-buddy --snap landing.png --at 2.35 --theme light --scale 2
```

The 760×360 point scene shows stable words leaving the mouth, the latest
provisional phrase beneath it, and the final result arriving in the field.
The landing positions use the field's measured font and content inset.
Review both themes and the complete recording, including the final hold.

Pure checks cover capture generations, accepted jobs surviving a new capture,
revised stable text, changed targets, bounded word count and timings, disabled
state, Reduce Motion, top/bottom placement, and shader validation including
pipeline overrides in both target colour spaces. The Labs row has light/dark
Linux goldens in the `mac-labs-buddy` scene.

This rehearsal proves the renderer and scripted state transitions. Native
microphone capture, the focused app's Accessibility geometry, click-through,
multiple displays, and OS Reduce Motion still need end-to-end observation.
A real dictation's partial timing depends on its selected streaming model.
