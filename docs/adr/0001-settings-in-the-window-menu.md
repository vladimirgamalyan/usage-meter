# 0001. Settings live in the window menu, not in a menu bar

- Status: Accepted
- Date: 2026-08-04

## Context

The window is a compact always-visible readout, a few rows of fill bars. It
carried a classic menu bar whose only item was **Settings**, which cost a full
row of vertical space on a window that is otherwise around 150 px tall. The very
same popup was already reachable through the right-click context menu over the
client area, so the bar was pure duplication.

## Decision

Drop the menu bar. The settings popup is inserted at the top of the window menu
(`GetSystemMenu`), which Windows surfaces through the title-bar icon, a
right-click on the caption and Alt+Space. The right-click context menu over the
client area stays as the second, faster path. Both are built by the same
`build_settings_popup`.

Menu command identifiers moved to 16-aligned values below `0xF000`: window menu
items arrive as `WM_SYSCOMMAND`, whose low four bits of `wParam` are reserved by
the system.

## Consequences

- The window loses a row of chrome and the menu bar no longer needs to be
  accounted for when fitting the window to its content.
- Settings are less discoverable than a visible menu bar: nothing on screen
  advertises them. Both entry points are standard Windows gestures for small
  utilities, and the README documents them.
- The whole menu is rebuilt on every settings change (checked items cannot be
  updated in place), now via `GetSystemMenu(hwnd, true)`, which destroys the
  previous copy along with our popup.
