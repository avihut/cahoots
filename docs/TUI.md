# Talking to a person: the TUI

cahoots asks a person something only at a terminal, and always in one style:
the **Clack rail**, after [Clack](https://github.com/bombshell-dev/clack). It
is the design for every interactive screen cahoots has or will have. The
rules below are its guidelines, and `src/tui` is its one implementation.

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

1. **A question is a selector, never typed in.** ↑ and ↓ (or ← and →, k and
   j, h and l) move, and wrap at the ends. Enter answers. Esc, Ctrl-C or
   Ctrl-D leaves. There is no "type 1, 2 or a name".
2. **Every question has a flag that answers it** (`install --meter …`), and
   is asked only when the flag isn't given. Agents and scripts never meet a
   question: they have no terminal, and the verbs that ask are human verbs,
   which refuse to run without one.
3. **Ask before doing.** A command asks everything it needs before it writes
   anything, so a question left unanswered leaves nothing half-done. It exits
   2 and names the flag that answers it.
4. **Draw on stderr only.** stdout is the one JSON envelope, as for every
   verb, so `cahoots install | jq` still works. A question needs stdin and
   stderr to be a terminal that can move its cursor (`TERM` not `dumb`).
   Without one it isn't asked, and the command names the flag instead.
5. **Give the terminal back.** Key-by-key input, the hidden cursor and the
   turned-off line wrap are restored however a question ends: Enter, Esc,
   Ctrl-C, or a panic. A release build aborts on a panic without unwinding,
   so a panic hook restores them.
6. **Color is a courtesy.** `NO_COLOR` turns it off. The shapes carry the
   meaning without it.
7. **No crates for it.** The rail is drawn by hand on `nix` (termios and
   poll), which cahoots already depends on, so the dependency list stays
   closed.

## The vocabulary

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

## The layers

The TUI is the interface, and only the interface (`AGENTS.md`, hard rule 11):

- **The logic** returns a question as data and takes the answer back as
  data. For install's meter, `Decision::Ask { options }` goes out, and
  `detect::picked(selection, …)` takes the answer back. It never draws, and
  never reads a key.
- **The command layer** words the question and says what each answer means
  (`src/cli/questions.rs`). It puts the question on the rail, and hands the
  answer to the logic.
- **The TUI** draws what it is given and says which choice was made. It
  knows nothing of meters, gates or runs, and uses nothing else in the crate.

Inside the TUI the same split holds, one file per layer. Only the first
touches a terminal:

| File | Holds |
|---|---|
| `terminal.rs` | the terminal: keys in as they are pressed, and everything given back on drop or panic |
| `keys.rs` | what a terminal's bytes mean (`Key`), and where keys come from (`Keys`) |
| `select.rs` | a prompt's state: which choice is highlighted, and when it is over |
| `view.rs` | how things look: the symbols, the colors, and a prompt's frame for its state |
| `rail.rs` | a conversation: its intro, each question drawn and redrawn as keys come, and its outro |

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

Never open the real terminal in a unit test. Under `cargo test` in a
terminal, stdin is the developer's own terminal, and key-by-key input would
take it over.
