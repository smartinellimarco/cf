const stdin = require('fs').readFileSync(0)
let memory
WebAssembly.instantiate(Buffer.from(WASM, 'base64'), {
  env: { write: (ptr, len) => process.stdout.write(Buffer.from(memory.buffer, ptr, len)) },
}).then(({ instance }) => {
  memory = instance.exports.memory
  const ptr = instance.exports.input(stdin.length)
  new Uint8Array(memory.buffer, ptr, stdin.length).set(stdin)
  instance.exports.run(ptr, stdin.length)
})
