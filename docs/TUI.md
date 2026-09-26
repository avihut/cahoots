# Talking to a person: the TUI

cahoots talks to a person only at a terminal, in one style with two shapes.
The rules below hold for both, and `src/tui` is their one implementation.

- **A question a command asks on its way** goes on the **Clack rail**, after
  [Clack](https://github.com/bombshell-dev/clack): a few lines in the
  terminal's own scrollback, and the command goes on. Install's usage meter
  is one.
- **The settings page** (`cahoots settings`) is a place, not a step: every
  setting on one list, which will only grow. It takes the whole screen, lets
  a person change one setting at a time in a box, and gives the screen back
  as it was.

```text
┌  cahoots install
│
◆  Which usage meter should cahoots use?
│  ● agent-usage (each plan's own percentages, as the vendors report them)
│  ○ ccusage
│  ○ none
└
```

Once answered, the question folds to its answer, and the rail closes on what
was decided:

```text
┌  cahoots install
│
◇  Which usage meter should cahoots use?
│  ccusage
│
└  Usage meter: ccusage
```

## Rules

1. **An answer is chosen, never typed in.** ↑ and ↓ (or k and j) move, and
   wrap at the ends; on the rail ← and → (h and l) move too, and on the page
   they step a number. Enter answers. Esc leaves what is open; Ctrl-C or
   Ctrl-D leaves altogether. There is no "type 1, 2 or a name".
2. **Every question has a flag that answers it** (`install --meter …`), and
   is asked only when the flag isn't given. The page has two:
   `settings set <key> <value>` and `settings reset <key>`. Agents and scripts
   never meet a question or the page: the verbs that ask are human verbs,
   which refuse to run without a terminal.
3. **Ask before doing.** A command asks everything it needs before it writes
   anything, so a question left unanswered leaves nothing half-done. It exits
   2 and names the flag that answers it. On the page each Enter in a box is a
   save, written at once; Esc leaves a box, and nothing is saved.
4. **Draw on stderr only.** stdout is the one JSON envelope, as for every
   verb, so `cahoots install | jq` still works. A question or the page needs
   stdin and stderr to be a terminal that can move its cursor (`TERM` not
   `dumb`). Without one, nothing is drawn, and the command names the flag.
5. **Give the terminal back.** Key-by-key input, the hidden cursor, the
   turned-off line wrap and, for the page, the alternate screen are restored
   however it ends: Enter, Esc, Ctrl-C, or a panic. A release build aborts on
   a panic without unwinding, so a panic hook restores them.
6. **Color is a courtesy.** `NO_COLOR` turns it off. The shapes carry the
   meaning without it: on the page, `•` marks a value set in config.toml.
7. **No crates for it.** Both are drawn by hand on `nix` (termios, poll, and
   a kqueue on macOS), which cahoots already depends on, so the dependency
   list stays closed.

## The rail's vocabulary

| Shape | Means | Color |
|---|---|---|
| `┌  title` | a conversation starts: the command that is asking | the `┌` gray |
| `│` | the rail every line hangs on | cyan while a question is open, gray once answered, red if left |
| `◆  question` | the question being asked | the `◆` cyan |
| `●  choice (hint)` | the highlighted choice, and what it means | the `●` green, the hint dim |
| `○  choice` | the other choices | dim |
| `◇  question` | a question answered; the line below is the answer | the `◇` green, the answer dim |
| `■  question` | a question left; the line below is where it was | the `■` red, the choice dim and struck through |
| `└  message` | the conversation's end: what was decided | the `└` gray, the message red if nothing was |

Text is left in the terminal's own color unless the table says otherwise.

Two spaces follow a symbol, and each choice takes one line. Only the
highlighted choice shows its hint.

A kind of question cahoots doesn't have yet is added to `src/tui` in the same
vocabulary, as Clack draws it: a yes/no question as `● Yes / ○ No`, a
multi-select with `◻` and `◼`, and a message on the rail as `●` (info), `▲`
(warning) or `■` (error).

## The settings page

```text
 cahoots settings                           ~/.config/cahoots/config.toml
──────────────────────────────────────────────────────────────────────────
 Claude Code
   Enabled              • on
 ❯ Usage cap              75%
   ╭─ Claude Code · Usage cap ─────────────────────────────────────────╮
   │ A run starts only while less than this much of Claude Code's plan │
   │ is used.                                                          │
   │                                                                   │
   │ ◀  65%  ▶                                                         │
   │ ━━━━━━━━━━━━━━━━━━━───────────                                    │
   │ default 75%                                                       │
   │                                                                   │
   │ ←→ change · r default · enter save · esc cancel                   │
   ╰───────────────────────────────────────────────────────────────────╯
```

Every setting is one line under its section's heading, and a box opens just
under the setting it changes (above it, near the bottom). Below the list are
the highlighted setting's help and where its value comes from. After a save,
the last line says what was written to config.toml, and the page reads the
file again, so an edit made by hand meanwhile shows too. Closing the page
prints what changed as the JSON envelope.

| Shape | Means | Color |
|---|---|---|
| `❯  label` | the highlighted setting | cyan |
| `• value` | set in config.toml | the value plain |
| `value` | a default, or what cahoots found | dim |
| a heading | a section: each harness, the usage meter, runs, review, roles | bold |
| `↑` `↓` at the right edge | more of the list above, or below | dim |
| `╭─ Section · Label ─╮` | the box that changes one setting: its help, the answer, the keys | gray edges |
| `●  choice (hint)` / `○  choice` / `✓` | a choice: the highlighted one, the others, the one chosen now | green, dim, gray |
| `◀  65%  ▶` and `━━━───` | a number, stepped with ← and →; the bar for a percent | cyan |
| `●  1. item` / `↕  1. item` | an order: the highlighted item; the item picked up to move | green, cyan |
| the last line | what the keys do; what a save wrote; why a save was refused | dim, green, red |

| Key | Does |
|---|---|
| ↑ ↓ (k j) | move between settings |
| Tab, Shift-Tab | the next section, the one before |
| Enter or Space | turn a toggle where it is; open the box for anything else |
| In a box: ↑ ↓ | choose, or move an item |
| In a box: ← → | step a number through round values: 5%, a minute, a million tokens |
| In a box: r | put a number's default back |
| In a box: Space | pick an item up, or put it down |
| In a box: Enter | save; a refusal stays in the box, in red |
| Esc | close the box, then the page (`q` too) |
| Ctrl-C | leave the page, from anywhere |

An answer that puts a setting back as it would be without one — its default,
or what was found — takes the key out of config.toml rather than write the
default into it. Choosing the usage meter is the exception: a meter chosen
is written even when it is the one found, because a person chose it.

### The whole screen

- **The alternate screen.** The page is drawn on it (`ESC [ ? 1049 h`), so
  closing it gives back the shell's screen as it was. The drop and the panic
  hook both leave it.
- **The size is asked of the terminal.** The cursor goes to the far corner,
  where the terminal stops it, and the terminal is asked where it is
  (`ESC [ 6 n` on stderr, answered on stdin as `ESC [ rows ; cols R`). With no
  answer within 300 ms, the page is drawn at 80×24. (`nix` has no safe call
  for the window size: the ioctl is unsafe code.)
- **A new size is heard.** A terminal says it changed size with SIGWINCH,
  which is ignored unless a handler is set, and setting one is unsafe code.
  On macOS a kqueue still counts it, and on Linux a thread takes it with
  `sigwait` while every other thread holds it back. Either way `pressed`
  watches it beside stdin, asks the size again, and the page is drawn at it.
- **Every frame is the whole screen**, drawn row by row from each row's
  start, so nothing scrolls and nothing wraps.

## The layers

The TUI is the interface, and only the interface (`AGENTS.md`, hard rule 11):

- **The logic** returns a question as data and takes the answer back as
  data. For install's meter, `Decision::Ask { options }` goes out, and
  `detect::picked(selection, …)` takes the answer back. For the page, every
  setting is data (`src/settings.rs`: its kind, value, default and where the
  value comes from), and a change goes back through `settings::set` or
  `settings::reset`. It never draws, and never reads a key.
- **The command layer** words the question or the settings and says what
  each answer means (`src/cli/questions.rs`, `src/cli/settings.rs`). It puts
  them on the rail or the page, and hands the answers to the logic.
- **The TUI** draws what it is given and says what was answered. It knows
  nothing of meters, gates, runs or settings, and uses nothing else in the
  crate.

Inside the TUI the same split holds, one file per layer. Only the first
touches a terminal:

| File | Holds |
|---|---|
| `terminal.rs` | the terminal: keys in as they are pressed, the whole screen and its size for the page, and everything given back on drop or panic |
| `keys.rs` | what a terminal's bytes mean (`Key`), and where keys come from (`Keys`) |
| `select.rs` | a choice: which one is highlighted, and when it is over |
| `stepper.rs` | a number: the round values it steps through, and how it reads |
| `order.rs` | an order: the highlighted item, and whether it is picked up |
| `page.rs` | the page: its rows, the highlighted one, the open box, and the events it sends |
| `view.rs` | how the rail looks: the symbols, the colors, and a prompt's frame for its state |
| `view/canvas.rs` | a grid to draw a whole screen on, as lines of text |
| `view/page.rs` | how the page looks: the list, the box, and the last line |
| `rail.rs` | a conversation: its intro, each question drawn and redrawn as keys come, and its outro |
| `screen.rs` | the page on the whole screen: drawn on every key and every new size, until an event |

`scripts/guard.sh` holds the boundaries. Only `src/main.rs`, `src/cli.rs`,
`src/cli/` and `src/tui` touch the terminal. `src/tui` uses nothing else in
the crate. Only the command layer uses `tui`.

## Adding a question

1. In the logic, return the question as data (a variant like
   `Decision::Ask`), and take the answer back through a function (like
   `detect::picked`). Test it without any TUI.
2. Give it a flag that answers it without asking.
3. Word it in `src/cli/questions.rs`: the prompt, the choices and their
   hints, what each choice means, the outro, the cancel line, and the error
   that names the flag. Test it with a script of keys and `Colors::OFF`, and
   read the screen back as text.
4. If it needs a new kind of prompt, add one to `src/tui`: its state in a
   file of its own, its frame in `view.rs`, run by `Rail`. Test each piece
   alone.
5. Run it once from end to end on a pseudo-terminal
   (`tests/common::AtTerminal`). Press the keys, then check the screen, the
   JSON on stdout, what was written, and that the terminal came back.

## Adding a setting

1. Give config.toml the key: a typed field in `src/config.rs`, its range as a
   constant there, held in `validate`. Nothing that could widen what cahoots
   may do is ever a key (hard rule 6).
2. Give it its default where the registry keeps defaults (`src/registry.rs`),
   so an empty config's registry has it.
3. Put it in the catalog, `src/settings.rs`: a `Key`, its name and section,
   and its `Setting` in `current` — its kind, its value now, its default,
   and where the value comes from. Test it there.
4. Word it in `src/cli/settings.rs`: its label and help, a choice's hints,
   and where a number starts stepping from nothing.
5. Run it once from end to end: through `settings set` and on the page
   (`tests/settings.rs`), and check what reached config.toml.

Never open the real terminal in a unit test. Under `cargo test` in a
terminal, stdin is the developer's own terminal, and key-by-key input would
take it over.
