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
