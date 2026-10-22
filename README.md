# cf

Rustlings-style Codeforces practice: run a list of problems, write the solution in your editor, and samples rerun on every save.

## Install

```
zig build -Doptimize=ReleaseFast --prefix ~/.local
```

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

`cf` prints the command to open the problem, built from `editor` in `cf.json` with `{file}` and `{dir}` filled in, e.g. `hx --vsplit {file} {dir}/in1.txt` or `vim -O {file} {dir}/in1.txt`.

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

`cf.json` picks the language and says how to build and run it. Commands run with `sh` inside the problem folder:

```json
"cpp": {
  "ext": "cpp",
  "comment": "//",
  "build": "clang++ -std=c++20 -O2 -o main main.cpp",
  "run": "./main",
  "submit": "main.cpp",
  "judge": "GNU G++20"
}
```

Codeforces has no Zig, so the `zig` language compiles the solution to WebAssembly and packs it into `submit.js`, which runs on the judge's Node.js 15.8. The samples run that same file. Write `solve(in, out)` in `main.zig` and submit `submit.js` as Node.js.
