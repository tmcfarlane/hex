# Hex buddy rehearsal

The native character follows the homepage's calm head shader. Word flight uses
Hex's existing onboarding renderer and OpenCode theme. The component uses Rune;
the scripted scene uses a real `rune-kit::TextInput` as its destination.

On macOS, rehearse the production character and word renderer without starting
a listener, microphone, Accessibility query, or settings store:

```sh
hex app -- --scene hex-buddy --video hex-buddy.mp4 --duration 5 --fps 60 --scale 2
hex app -- --scene hex-buddy --snap speaking.png --at 1.35 --theme dark --scale 2
hex app -- --scene hex-buddy --snap landing.png --at 2.65 --theme light --scale 2
hex app -- --scene hex-buddy --snap final.png --at 3.7 --theme dark --scale 2
```

The 760×360 point scene updates the real text field one word at a time, then
acknowledges each insertion with a flight from the mouth to that word's slot.
Tentative text stays beneath the character. Releasing leaves the field intact
without replaying a second batch. Landing positions use the field's measured
`ui_compact` font and its content inset at (296, 237). Review both themes and
the complete recording, including the final hold.

Pure checks cover no flight before an insertion acknowledgement, one flight per
confirmed word, stale captures, accepted jobs surviving a new capture, revised
stable text, caret movement, disabled state, Reduce Motion, top/bottom placement,
and keeping the real field through release. Shader checks include pipeline
overrides in both target colour spaces. The Labs row has light/dark Linux
goldens in the `mac-labs-buddy` scene.

This rehearsal proves the renderer, real Rune text-field updates, and scripted
state transitions. It does not exercise the live Accessibility editing path.
Native microphone capture, the focused app's actual inserted text and geometry,
click-through, multiple displays, and OS Reduce Motion still need end-to-end
observation. A real dictation's partial timing depends on its streaming model.
