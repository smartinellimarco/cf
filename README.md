# cf

Rustlings-style Codeforces practice: run a list of problems, write the solution in your editor, and samples rerun on every save.

## Install

```
brew install smartinellimarco/tap/cf
```

Or with the shell installer from the [latest release](https://github.com/smartinellimarco/cf/releases/latest), or from source with `cargo install --path .`.

## Use

```
mkdir codeforces && cd codeforces
cf 4A
```

The first run writes the config to `~/.config/cf/` and asks for your Codeforces handle, which it checks against Codeforces before saving.

`cf` walks a list of problems. The list decides which problems show; the solutions in the folder fill in how your samples do and your Codeforces submissions fill in the verdicts, for whatever problems they share with the list.

- `cf` walks the solutions in the folder, from your last submission among them.
- `cf 4A 71A 1C` walks those problems, ids or links, in that order. `cf 4A` is a list of one.
- `cf -l <list>` walks a list from a file, a URL, stdin or `config.json`.
- `cf -d <dir>` takes the solutions from `<dir>` instead of the current folder.
- `cf 4A --lang python` starts solutions in another language than `default_lang`.
- `cf --no-cache` starts with an empty cache that goes away on exit, to try things from scratch.

A list starts at its first problem you have not solved. Gym problems and anything outside the Codeforces problemset are left out.

A solution is a file named `<id>.<ext>`, like `4A.cpp`, anywhere under the solutions folder; `.gitignore`, `.ignore` and hidden files are skipped. Moving between problems only downloads their statements and samples; `e` starts a solution, in the folder itself with the statement as a comment on top, and you can move it anywhere after. A problem can have one solution per language, and with the same name twice the most recently edited one counts. Solved and tried come from your Codeforces submissions; the files only say how your samples do.

The samples live in the cache, under `~/.cache/cf/samples/<id>/` as `in<k>.txt` and `ans<k>.txt`. Add your own cases there with the next free `k`; they go away with the cache. Old problems that read `input.txt` and write `output.txt` get their samples through those files, and `cf` says so on screen.

`cf` prints the command to open the problem, built from `editor` in the config, e.g. `hx --vsplit {source} {samples}/in1.txt` or `vim -O {source} {samples}/in1.txt`.

The progress grid has a column per problem. Its first row is always local: the result of the solution on screen, green when the samples pass, red when they fail, a filling circle while they run, dim when the problem has no samples, and a small circle when there is no solution here. Above it go your submissions, of any language, newest first: green accepted, red rejected, gray compilation error, yellow still testing, the last five shown and a mark on top when there are older ones. The grid is one band as wide as the terminal that slides over the list as the cursor nears an edge, with the count of problems left and right of it at its ends. Under the grid, the legend counts how many problems of the whole list are in each state. `cf` runs the samples of every solution in the background and reruns only what changed. The grid and the language icons are [Nerd Fonts](https://www.nerdfonts.com/) glyphs, in your terminal's colors.

Keys: `h`/`l` go to the previous or next problem and `j`/`k` move between a column's rows, its local result at the bottom and its submissions above; a popup shows the cell under the cursor (judge, verdict, failing test, time and memory, or your solutions here); `i` shows or hides it and `esc` hides it. `gg`/`ge` go to the first or last problem, `zz` centers the grid on the current one, `]f`/`[f` to the next or previous one whose newest submission was rejected, `]s`/`[s` accepted, `]u`/`[u` never submitted, and by the solution here, `]e`/`[e` failing its samples, `]p`/`[p` passing them, `]t`/`[t` with no solution yet. `y` copies the file to submit, `o` opens the submit page, `w` opens the problem on Codeforces, `r` reruns, `s` swaps the solution, `e` starts one, `q` quits.

## Lists

A list is any text that mentions problems, in order. `cf` picks up bare ids and Codeforces links:

```
4A
1352C
https://codeforces.com/problemset/problem/1374/B
https://codeforces.com/contest/1520/problem/F2
```

`-l` takes a file, a URL, `-` for stdin, a folder, or the name of a list in `config.json`. A folder lists the solutions in it, so `cf -l ~/old-solutions` walks those:

```
cf -l ladder.txt
cf -l ~/old-solutions
cf -l https://example.com/ladder-1200
pbpaste | cf -l -
cf -l div2a
```

Named lists map a name to a file or a URL:

```json
"lists": {
  "div2a": "https://raw.githubusercontent.com/karansinghgit/CodeForces-Ladders/master/div2A.md"
}
```

The default config comes with the [A2OJ ladders](https://github.com/rishabhdeepsingh/A2OJ-Ladder) by rating, `a2oj-1200` to `a2oj-1900`, and the [updated ladders](https://github.com/karansinghgit/CodeForces-Ladders) by division and topic: `div2a` to `div2e`, `dp` and `strings`.

## Config

`cf` follows the XDG base directories, so `XDG_CONFIG_HOME` and `XDG_CACHE_HOME` move these:

| Path | Holds |
|---|---|
| `~/.config/cf/config.json` | handle, editor and languages |
| `~/.config/cf/templates/main.<ext>` | the code every new problem starts from |
| `~/.cache/cf/` | the problem set, your submissions, samples, results and builds |

Edit `templates/main.cpp` to start every solution with your own boilerplate: includes, macros, fast IO. A new language needs its own `templates/main.<ext>`. Solutions already started keep the code they started with.

## Keys

`keys+` in `config.json` adds keys or rebinds some, and `keys` replaces every default key with its own. A key is one or two characters, like `q`, `gg` or `]f`:

```json
"keys+": { "n": "next", "p": "prev" }
```

The commands are `next`, `prev`, `up`, `down`, `first`, `last`, `center`, `next_failed`, `prev_failed`, `next_solved`, `prev_solved`, `next_unsubmitted`, `prev_unsubmitted`, `next_local_error`, `prev_local_error`, `next_local_pass`, `prev_local_pass`, `next_untouched`, `prev_untouched`, `copy`, `submit`, `web`, `start`, `swap`, `rerun`, `info` and `quit`. `esc` hides the popup and `ctrl-c` quits, whatever the keys.

## Languages

`config.json` says how to build and run each language, and `default_lang` picks the one new solutions use:

```json
"default_lang": "cpp",
"langs": {
  "cpp": {
    "ext": "cpp",
    "icon": "\ue61d",
    "color": "blue",
    "comment": "//",
    "build": "clang++ -std=c++20 -O2 -o main {source}",
    "run": "./main",
    "submit": "{source}"
  }
}
```

- `ext` is the extension of the solution files and names the template they start from. Two languages can't share one.
- `icon` is a [Nerd Fonts](https://www.nerdfonts.com/cheat-sheet) glyph shown in the local popup, and `color` one of the terminal's colors for it: black, red, green, yellow, blue, magenta, cyan, white, grey, or their `dark_` versions.
- `comment` prefixes the statement copied on top of the solution.
- `build` and `run` are `sh` commands that run inside the solution's build folder in the cache, so whatever they write stays out of the solutions folder. `run` gets each sample on stdin.
- `submit` is the file `y` copies. It can be the source or something the build made.

Commands and `editor` can use these placeholders, which `cf` fills in with absolute paths quoted for `sh`:

| Placeholder | Path |
|---|---|
| `{source}` | the solution file |
| `{samples}` | the problem's samples, under `~/.cache/cf/samples/` |
| `{build}` | the solution's build folder, under `~/.cache/cf/build/` |

`cf` rejects unknown placeholders when it loads the config. If a build leaves a file next to the source anyway, `cf` warns about it.
