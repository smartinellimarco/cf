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

- `cf` alone opens the problem of your last submission.
- `cf <problem>` opens a problem by id or link and watches it.

Without a list, `cf` walks a line in id order: the problems you submitted to, then the whole problemset from where you started. `p` goes back over what you did and `n` moves on.
- `cf -l <list>` walks a list from its first problem you have not solved. Add a problem to open the list there.
- `cf -d <dir>` takes the solutions from `<dir>` instead of the current folder.
- `cf <problem> --lang python` starts a solution in another language than `default_lang`.
- `cf sync` refreshes the problem set and your submissions.

Gym problems and anything outside the Codeforces problemset are left out.

A solution is a file named `<id>.<ext>`, like `4A.cpp`, anywhere under the solutions folder; `.gitignore`, `.ignore` and hidden files are skipped. Moving between problems only downloads their statements and samples; `e` starts a solution, in the folder itself with the statement as a comment on top, and you can move it anywhere after. A problem can have one solution per language, and with the same name twice the most recently edited one counts. Solved and tried come from your Codeforces submissions; the files only say how your samples do.

The samples live in the cache, under `~/.cache/cf/samples/<id>/` as `in<k>.txt` and `ans<k>.txt`. Add your own cases there with the next free `k`; they go away with the cache. Old problems that read `input.txt` and write `output.txt` get their samples through those files, and `cf` says so on screen.

`cf` prints the command to open the problem, built from `editor` in the config, e.g. `hx --vsplit {source} {samples}/in1.txt` or `vim -O {source} {samples}/in1.txt`.

The progress grid has a column per problem with your submissions stacked upwards, the last five shown. It counts solved over the list, or over the problems you tried when there is no list. A ring is a submission, of any language: green accepted, red rejected, gray compilation error, yellow still testing. A dot on top is the local result of the solution on screen when it is newer than your last submission: green when the samples pass, red when they fail, dim while they run. A cross on the baseline marks a problem with solutions in several languages, and `s` swaps between them. `cf` runs the samples of every solution in the background and reruns only what changed.

Keys: `n`/`p` go to the next or previous problem, `gg`/`ge` to the first or last, `]f`/`[f` to the next or previous failed one, `]s`/`[s` solved, `]t`/`[t` untouched. `y` copies the file to submit, `o` opens the submit page, `l` lists the problems (the same motions move the cursor, `c` opens the one under it), `r` reruns, `s` swaps the solution, `e` starts one, `q` quits.

## Lists

A list is any text that mentions problems, in order. `cf` picks up bare ids and Codeforces links:

```
4A
1352C
https://codeforces.com/problemset/problem/1374/B
https://codeforces.com/contest/1520/problem/F2
```

`-l` takes a file, a URL, `-` for stdin, a folder, or the name of a list in `config.json`. A folder lists the solutions in it, so `cf -l .` walks what you have here:

```
cf -l ladder.txt
cf -l .
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

## Languages

`config.json` says how to build and run each language, and `default_lang` picks the one new solutions use:

```json
"default_lang": "cpp",
"langs": {
  "cpp": {
    "ext": "cpp",
    "icon": "\ue61d",
    "comment": "//",
    "build": "clang++ -std=c++20 -O2 -o main {source}",
    "run": "./main",
    "submit": "{source}"
  }
}
```

- `ext` is the extension of the solution files and names the template they start from. Two languages can't share one.
- `icon` is a [Nerd Fonts](https://www.nerdfonts.com/cheat-sheet) glyph shown next to the file name.
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
