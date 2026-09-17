import fs from "node:fs";

const helperPath = process.argv[2];
if (!helperPath) {
  console.error("usage: patch_threaded_worker_helper.mjs HELPER.js");
  process.exitCode = 2;
} else {
  const source = fs.readFileSync(helperPath, "utf8");
  const deprecated = "await pkg.default(data.module, data.memory);";
  const current = "await pkg.default({ module_or_path: data.module, memory: data.memory });";
  if (source.includes(deprecated)) {
    fs.writeFileSync(helperPath, source.replace(deprecated, current));
  } else if (!source.includes(current)) {
    console.error("wasm-bindgen-rayon helper initialization call was not recognized");
    process.exitCode = 1;
  }
}
