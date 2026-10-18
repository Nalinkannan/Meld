# Meld — UI Design

## Design principles

The TUI is a first-class product requirement. It is information-dense but not
cluttered, with excellent alignment, hierarchy, spacing, keyboard navigation,
and consistent visual feedback. It is a full-screen Ratatui application
launched by default.

## Design system

All colors, styles, symbols, borders, and component styling live in
`crates/meld-tui/src/theme.ts` (theme.rs). Arbitrary color literals are never
scattered across screens.

Initial palette (Dracula-inspired design system tokens):

| Token | RGB | Use |
|-------|-----|-----|
| BG | 24,24,37 | Background |
| PANEL | 33,33,48 | Panel background |
| PANEL_ALT | 42,42,60 | Alternate panel |
| BORDER | 74,78,105 | Borders |
| ACCENT | 189,147,249 | Primary accent |
| ACCENT_SECONDARY | 139,233,253 | Secondary accent |
| SUCCESS | 80,250,123 | Success |
| WARNING | 255,184,108 | Warning |
| DANGER | 255,85,85 | Error/destructive |
| TEXT | 248,248,242 | Body text |
| MUTED | 145,147,170 | Muted text |
| ADD_DIM / REMOVE_DIM / MODIFY_DIM | — | Diff colors |

Semantic style functions exist for success, warning, error, focus, addition,
removal, and modification. Color is never used alone to communicate state;
colors are paired with text or symbols.

## Layout

- Compact branded header (Meld + active engine).
- Central workspace.
- Contextual navigation / action area.
- Persistent status bar: connection, branch, HEAD, keyboard hints.
- Consistent modals for confirmation, errors, help, conflict resolution.

Narrow terminals are handled gracefully (no overlap, panic, or unreadable
content on resize).

## Keyboard model

- Arrows / `j`/`k`: navigate lists.
- Enter: activate or inspect.
- Tab / Shift+Tab: move focus.
- Esc: dismiss modal / previous screen.
- `?`: help.
- `q`: safe quit confirmation.
- Contextual shortcuts for snapshot, history, diff, merge, branches.

Printable keys never trigger navigation while an input is focused.

## Screen states

Every screen has a loading state, empty state, error state, and success state
as appropriate. No screen contains fake data or placeholder success messages.
