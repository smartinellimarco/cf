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
cf init <handle>
cf lists/ladder.txt
```

- `cf init <handle>` writes `cf.json`, the language templates and the Zig runtime into the current folder.
- `cf <list>` opens the first problem of the list you have not solved and watches it.
- `cf` alone reopens the last list.
- `cf <list> --fresh` restarts the list: only accepted submissions from now on count.
- `cf sync` refreshes the problem set and your submissions.

Each problem lives in `problems/<id>/`: `main.<ext>` with the statement as a comment on top, and `in<k>.txt`/`ans<k>.txt` samples. Add your own cases with the next free `k`.

`cf` prints the command to open the problem, built from `editor` in `cf.json`, e.g. `hx --vsplit {source} {dir}/in1.txt` or `vim -O {source} {dir}/in1.txt`.

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

## Languages

`cf.json` picks the language and says how to build and run it:

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

- `ext` names the solution file, `main.<ext>`, and `templates/main.<ext>` is what a new problem starts from.
- `comment` prefixes the statement copied on top of the solution.
- `build` and `run` are `sh` commands that run inside `problems/<id>/build/`, so whatever they write lands there, and `.gitignore` ignores that folder for every language. `run` gets each sample on stdin.
- `submit` is the file `y` copies. It can be the source or something the build made.
- `judge` is the Codeforces language to pick when submitting.

Commands and `editor` can use these placeholders, which `cf` fills in with absolute paths quoted for `sh`:

| Placeholder | Path |
|---|---|
| `{source}` | `problems/<id>/main.<ext>` |
| `{dir}` | `problems/<id>` |
| `{build}` | `problems/<id>/build` |
| `{root}` | the folder with `cf.json` |
| `{cache}` | `cf`'s cache, `~/.cache/cf`, shared by every problem |

`cf` rejects unknown placeholders when it loads `cf.json`. If a build leaves a file next to the source anyway, `cf` warns that git won't ignore it.

Codeforces has no Zig, so the `zig` language compiles the solution to WebAssembly and packs it into `submit.js`, which runs on the judge's Node.js 15.8. The samples run that same file. Write `solve(in, out)` in `main.zig` and submit `submit.js` as Node.js.
