import fs from "node:fs";

function readVarUint(bytes, state) {
  let value = 0;
  let shift = 0;
  while (state.offset < bytes.length && shift < 35) {
    const byte = bytes[state.offset++];
    value |= (byte & 0x7f) << shift;
    if ((byte & 0x80) === 0) return value >>> 0;
    shift += 7;
  }
  throw new Error("malformed Wasm integer");
}

function readName(bytes, state) {
  const length = readVarUint(bytes, state);
  const end = state.offset + length;
  if (end > bytes.length) throw new Error("malformed Wasm name");
  const name = new TextDecoder().decode(bytes.subarray(state.offset, end));
  state.offset = end;
  return name;
}

function readLimits(bytes, state) {
  const flags = readVarUint(bytes, state);
  readVarUint(bytes, state);
  if (flags & 1) readVarUint(bytes, state);
  return { shared: (flags & 2) !== 0 };
}

function readSection(bytes, state, end, id) {
  if (id === 2) {
    const count = readVarUint(bytes, state);
    const imports = [];
    for (let index = 0; index < count; index += 1) {
      const module = readName(bytes, state);
      const name = readName(bytes, state);
      const kind = bytes[state.offset++];
      if (kind === 0) {
        readVarUint(bytes, state);
      } else if (kind === 1) {
        state.offset += 1;
        readLimits(bytes, state);
      } else if (kind === 2) {
        imports.push({ module, name, ...readLimits(bytes, state) });
      } else if (kind === 3) {
        state.offset += 2;
      } else {
        throw new Error("unsupported Wasm import kind");
      }
    }
    return { imports };
  }
  if (id === 5) {
    const count = readVarUint(bytes, state);
    for (let index = 0; index < count; index += 1) readLimits(bytes, state);
    return { definedMemories: count };
  }
  if (id === 7) {
    const count = readVarUint(bytes, state);
    const exports = [];
    for (let index = 0; index < count; index += 1) {
      const name = readName(bytes, state);
      const kind = bytes[state.offset++];
      const exportIndex = readVarUint(bytes, state);
      if (kind === 2) exports.push({ name, index: exportIndex });
    }
    return { exports };
  }
  state.offset = end;
  return {};
}

export function validateThreadedWorkerArtifacts(glueSource, wasmBytes) {
  let module;
  try {
    module = new WebAssembly.Module(wasmBytes);
  } catch (error) {
    throw new Error(`threaded worker Wasm is invalid: ${error}`);
  }

  const bytes = new Uint8Array(wasmBytes);
  const state = { offset: 8 };
  const memoryImports = [];
  const memoryExports = [];
  let definedMemories = 0;
  while (state.offset < bytes.length) {
    const id = bytes[state.offset++];
    const length = readVarUint(bytes, state);
    const end = state.offset + length;
    if (end > bytes.length) throw new Error("malformed Wasm section");
    const section = readSection(bytes, state, end, id);
    if (section.imports) memoryImports.push(...section.imports);
    if (section.exports) memoryExports.push(...section.exports);
    definedMemories += section.definedMemories ?? 0;
    if (state.offset !== end) throw new Error("malformed Wasm section");
  }
  if (memoryImports.length !== 1 || definedMemories !== 0) {
    throw new Error("threaded worker Wasm must import exactly one memory");
  }
  if (!memoryImports[0].shared) {
    throw new Error("threaded worker Wasm memory import must be shared");
  }
  if (memoryExports.length !== 1 || memoryExports[0].name !== "memory" || memoryExports[0].index !== 0) {
    throw new Error("threaded worker Wasm must export its imported memory as memory");
  }
  if (!/function __wbg_get_imports\s*\(\s*memory\s*\)/.test(glueSource)) {
    throw new Error("threaded worker glue does not accept the shared memory argument");
  }
  if (!/new WebAssembly\.Memory\(\s*\{[\s\S]*shared\s*:\s*true/.test(glueSource)) {
    throw new Error("threaded worker glue does not create shared memory");
  }
  if (!/async function __wbg_init\s*\([^)]*\bmemory\b/.test(glueSource)) {
    throw new Error("threaded worker glue does not pass shared memory to initialization");
  }
  return memoryImports[0];
}

if (process.argv[1] === new URL(import.meta.url).pathname) {
  const [gluePath, wasmPath] = process.argv.slice(2);
  if (!gluePath || !wasmPath) {
    console.error("usage: verify_threaded_worker.mjs GLUE.js MODULE.wasm");
    process.exitCode = 2;
  } else {
    try {
      const memory = validateThreadedWorkerArtifacts(
        fs.readFileSync(gluePath, "utf8"),
        fs.readFileSync(wasmPath),
      );
      console.log(`threaded worker memory import: ${memory.module}.${memory.name}`);
    } catch (error) {
      console.error(String(error));
      process.exitCode = 1;
    }
  }
}
