// Compile the Ink TUI to standalone binaries for each platform.
import { $ } from "bun";

const targets = [
  { target: "bun-darwin-arm64", out: "prism-tui-darwin-arm64" },
  { target: "bun-darwin-x64", out: "prism-tui-darwin-x64" },
  { target: "bun-linux-x64", out: "prism-tui-linux-x64" },
  { target: "bun-linux-arm64", out: "prism-tui-linux-arm64" },
];

const buildTarget = process.argv[2]; // optional: build only one target

for (const { target, out } of targets) {
  if (buildTarget && !target.includes(buildTarget)) continue;
  console.log(`Building ${target}...`);
  await $`bun build src/index.tsx --compile --target=${target} --outfile dist/${out}`;
}

console.log("Done.");
