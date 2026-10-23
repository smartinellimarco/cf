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

The first run writes the config to `~/.config/cf/` and asks for your Codeforces handle, which it checks against Codeforces before saving. Any folder works as a solutions folder; `cf` keeps `problems/` in it and nothing else.

- `cf <problem>` opens a problem by id or link and watches it. `n` walks the whole problemset from there, oldest contest first.
- `cf -l <list>` walks a list from its first problem you have not solved. Add a problem to open the list there.
- `cf` alone reopens what was open when you quit.
- `cf sync` refreshes the problem set and your submissions.

What counts as solved comes from your Codeforces submissions, so a problem solved once shows as solved in every list. Each problem has one solution, shared by every list that has it.

Each problem lives in `problems/<id>/`: `main.<ext>` with the statement as a comment on top, and `in<k>.txt`/`ans<k>.txt` samples. Add your own cases with the next free `k`. Old problems that read `input.txt` and write `output.txt` get their samples through those files, and `cf` says so on screen.

`cf` prints the command to open the problem, built from `editor` in the config, e.g. `hx --vsplit {source} {dir}/in1.txt` or `vim -O {source} {dir}/in1.txt`.

The progress grid has a column per problem with your attempts stacked upwards, the last five shown. A ring is a submission: green accepted, red rejected, gray compilation error, yellow still testing, filled when sent from `cf` while the samples passed. A dot on top is the local result newer than your last submission: green when the samples pass, red when they fail, dim while they run. `cf` runs the samples of every problem in the folder in the background and reruns only what changed.

Keys: `y` copies the file to submit, `o` opens the submit page, `l` lists the problems, `n` jumps to the next unsolved one, `r` reruns, `q` quits.

## Lists

A list is any text that mentions problems, in order. `cf` picks up bare ids and Codeforces links:

```
4A
1352C
https://codeforces.com/problemset/problem/1374/B
https://codeforces.com/contest/1520/problem/F2
```

`-l` takes a file, a URL, `-` for stdin, or the name of a list in `config.json`:

```
cf -l ladder.txt
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
| `~/.cache/cf/` | the problem set, your submissions, what each folder had open and builds |

Edit `templates/main.cpp` to start every problem with your own boilerplate: includes, macros, fast IO. A new language needs its own `templates/main.<ext>`. Problems already opened keep the code they started with.

## Languages

`config.json` picks the language and says how to build and run it:

```json
"cpp": {
  "ext": "cpp",
  "comment": "//",
  "build": "clang++ -std=c++20 -O2 -o main {source}",
  "run": "./main",
  "submit": "{source}",
  "judge": "GNU G++20"
}
```

- `ext` names the solution file, `main.<ext>`, and the template it starts from.
- `comment` prefixes the statement copied on top of the solution.
- `build` and `run` are `sh` commands that run inside the problem's build folder in the cache, so whatever they write stays out of the solutions folder. `run` gets each sample on stdin.
- `submit` is the file `y` copies. It can be the source or something the build made.
- `judge` is the Codeforces language to pick when submitting.

Commands and `editor` can use these placeholders, which `cf` fills in with absolute paths quoted for `sh`:

| Placeholder | Path |
|---|---|
| `{source}` | `problems/<id>/main.<ext>` |
| `{dir}` | `problems/<id>` |
| `{build}` | the problem's build folder, under `~/.cache/cf/build/` |

`cf` rejects unknown placeholders when it loads the config. If a build leaves a file next to the source anyway, `cf` warns about it.
