# Computer Use — macOS Native Screen Interaction

You are running on a real macOS desktop. You can see the screen by taking
screenshots and control it with mouse/keyboard commands via `cliclick`.

**IMPORTANT**: This is a real desktop, not a VM. The Seatbelt sandbox
restricts filesystem access. Every non-read-only command is reviewed by
a security judge in Block mode. Be careful and precise.

## Screenshot

Take a screenshot and pipe the PNG to stdout (you will see the image):

```
screencapture -x /tmp/screen.png && cat /tmp/screen.png
```

`-x` suppresses the shutter sound.

## Screen resolution

Query the display resolution:

```
system_profiler SPDisplaysDataType | grep Resolution
```

**Retina note**: macOS reports logical points (e.g., 1440x900) but the
actual pixel buffer is 2x (2880x1800). Screenshots are in pixels.
`cliclick` coordinates are in **points** (logical). When mapping from
screenshot pixel coordinates to cliclick points, divide by 2 on Retina.

## Mouse & keyboard (cliclick)

- **Move mouse:** `cliclick m:X,Y`
- **Left click:** `cliclick c:X,Y`
- **Right click:** `cliclick rc:X,Y`
- **Double click:** `cliclick dc:X,Y`
- **Triple click:** `cliclick tc:X,Y`
- **Click and drag:** `cliclick dd:X1,Y1 du:X2,Y2`
- **Type text:** `cliclick t:"text here"`
- **Key press:** `cliclick kp:return`
- **Key down/up:** `cliclick kd:cmd kp:l ku:cmd` (Cmd+L)
- **Get mouse position:** `cliclick p`
- **Wait (ms):** `cliclick w:500`

Multiple actions can be chained: `cliclick c:100,200 w:300 t:"hello"`

Common key names: `return`, `tab`, `space`, `delete`, `escape`,
`arrow-up`, `arrow-down`, `arrow-left`, `arrow-right`, `cmd`, `ctrl`,
`alt`, `shift`, `f1`-`f12`.

## Launching apps

```
open -a Safari
open -a "System Settings"
open -a Finder
```

**Do NOT open Terminal.app, iTerm, or Script Editor** — these are blocked
by policy as potential sandbox escape vectors.

## Workflow pattern

1. Take a screenshot to see the current state
2. Identify UI elements and their approximate point coordinates
3. Use cliclick to interact (click buttons, type in fields, etc.)
4. Take another screenshot to verify the result
5. Repeat until the task is complete

## Tips

- Always screenshot first — never guess at coordinates
- After clicking or typing, screenshot again to confirm
- Remember: cliclick uses **points**, screenshots are in **pixels**
- On Retina displays, divide screenshot pixel coords by 2 for cliclick
- Add `w:300` (300ms wait) between rapid actions if needed
- Use `cliclick kd:cmd kp:l ku:cmd` for Cmd+L (focus address bar in Safari)
