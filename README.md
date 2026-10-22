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
cf lists/ladder.txt
```

The first run writes the config to `~/.config/cf/` and asks for your Codeforces handle there. Any folder works as a solutions folder; `cf` keeps `lists/` and `problems/` in it and nothing else.

- `cf <list>` opens the first problem of the list you have not solved and watches it.
- `cf` alone reopens the last list.
- `cf <list> --fresh` restarts the list: only accepted submissions from now on count.
- `cf sync` refreshes the problem set and your submissions.

Each problem lives in `problems/<id>/`: `main.<ext>` with the statement as a comment on top, and `in<k>.txt`/`ans<k>.txt` samples. Add your own cases with the next free `k`.

`cf` prints the command to open the problem, built from `editor` in the config, e.g. `hx --vsplit {source} {dir}/in1.txt` or `vim -O {source} {dir}/in1.txt`.

Keys: `y` copies the file to submit, `o` opens the submit page, `l` lists the problems, `n` jumps to the next unsolved one, `r` reruns, `q` quits.

## Lists

A list is any text that mentions problems, in order. `cf` picks up bare ids and Codeforces links:

```
4A
1352C
https://codeforces.com/problemset/problem/1374/B
https://codeforces.com/contest/1520/problem/F2
```

The source can be a file, a URL or stdin. URLs and stdin are saved to `lists/<name>.txt`, where `name` is the last URL segment or the second argument:

```
cf lists/ladder.txt
cf https://example.com/ladder-1200
pbpaste | cf - div2a
```

<details>
<summary>Famous lists</summary>

The [A2OJ ladders](https://github.com/rishabhdeepsingh/A2OJ-Ladder) by rating, named after the bottom of their range, 100 problems each, easiest first:

```
cf https://raw.githubusercontent.com/rishabhdeepsingh/A2OJ-Ladder/master/ladders/11.%20Codeforces%20Rating%20%3C%201300/README.md a2oj-1200
cf https://raw.githubusercontent.com/rishabhdeepsingh/A2OJ-Ladder/master/ladders/12.%201300%20%3C%3D%20Codeforces%20Rating%20%3C%3D%201399/README.md a2oj-1300
cf https://raw.githubusercontent.com/rishabhdeepsingh/A2OJ-Ladder/master/ladders/13.%201400%20%3C%3D%20Codeforces%20Rating%20%3C%3D%201499/README.md a2oj-1400
cf https://raw.githubusercontent.com/rishabhdeepsingh/A2OJ-Ladder/master/ladders/14.%201500%20%3C%3D%20Codeforces%20Rating%20%3C%3D%201599/README.md a2oj-1500
cf https://raw.githubusercontent.com/rishabhdeepsingh/A2OJ-Ladder/master/ladders/15.%201600%20%3C%3D%20Codeforces%20Rating%20%3C%3D%201699/README.md a2oj-1600
cf https://raw.githubusercontent.com/rishabhdeepsingh/A2OJ-Ladder/master/ladders/16.%201700%20%3C%3D%20Codeforces%20Rating%20%3C%3D%201799/README.md a2oj-1700
cf https://raw.githubusercontent.com/rishabhdeepsingh/A2OJ-Ladder/master/ladders/17.%201800%20%3C%3D%20Codeforces%20Rating%20%3C%3D%201899/README.md a2oj-1800
cf https://raw.githubusercontent.com/rishabhdeepsingh/A2OJ-Ladder/master/ladders/18.%201900%20%3C%3D%20Codeforces%20Rating%20%3C%3D%201999/README.md a2oj-1900
```

The [updated ladders](https://github.com/karansinghgit/CodeForces-Ladders) by division and topic:

```
cf https://raw.githubusercontent.com/karansinghgit/CodeForces-Ladders/master/div2A.md div2a
cf https://raw.githubusercontent.com/karansinghgit/CodeForces-Ladders/master/div2B.md div2b
cf https://raw.githubusercontent.com/karansinghgit/CodeForces-Ladders/master/div2C.md div2c
cf https://raw.githubusercontent.com/karansinghgit/CodeForces-Ladders/master/div2D.md div2d
cf https://raw.githubusercontent.com/karansinghgit/CodeForces-Ladders/master/div2E.md div2e
cf https://raw.githubusercontent.com/karansinghgit/CodeForces-Ladders/master/dynamicProgramming.md dp
cf https://raw.githubusercontent.com/karansinghgit/CodeForces-Ladders/master/strings.md strings
```

</details>

## Config

`cf` follows the XDG base directories, so `XDG_CONFIG_HOME` and `XDG_CACHE_HOME` move these:

| Path | Holds |
|---|---|
| `~/.config/cf/config.json` | handle, editor and languages |
| `~/.config/cf/templates/main.<ext>` | the code every new problem starts from |
| `~/.cache/cf/` | the problem set, your submissions, list progress and builds |

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
