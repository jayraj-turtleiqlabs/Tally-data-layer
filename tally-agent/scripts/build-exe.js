import { execSync } from "child_process";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const rootDir = path.resolve(__dirname, "..");
const outDir = path.join(rootDir, "release-exe");

console.log("\n=======================================================");
console.log(" 🚀 Building Single-File Standalone Production EXE");
console.log("=======================================================\n");

// Ensure clean output dir
if (!fs.existsSync(outDir)) {
  fs.mkdirSync(outDir, { recursive: true });
}

// 1. Build frontend
console.log("[1/3] Building frontend assets (Vite + TypeScript)...");
execSync("npm run build", { cwd: rootDir, stdio: "inherit" });

// 2. Build Tauri release (Bundles Rust + Embedded Frontend into single binary)
console.log("\n[2/3] Compiling Tauri release binary & bundle...");
execSync("npx tauri build", { cwd: rootDir, stdio: "inherit" });

// 3. Extract standalone single EXE to clean output folder
console.log("\n[3/3] Exporting standalone single EXE to ./release-exe/ ...");

const releaseDir = path.join(rootDir, "src-tauri", "target", "release");
const nsisDir = path.join(releaseDir, "bundle", "nsis");

// Direct standalone single executable
const directExe = path.join(releaseDir, "fininsight-tally-agent.exe");
if (fs.existsSync(directExe)) {
  const destPath = path.join(outDir, "fininsight-tally-agent.exe");
  fs.copyFileSync(directExe, destPath);
  const sizeMb = (fs.statSync(destPath).size / (1024 * 1024)).toFixed(2);
  console.log(` ✅ Standalone One-File EXE: ./release-exe/fininsight-tally-agent.exe (${sizeMb} MB)`);
}

// NSIS Single-File Installer if present
if (fs.existsSync(nsisDir)) {
  const files = fs.readdirSync(nsisDir).filter((f) => f.endsWith(".exe"));
  for (const file of files) {
    const src = path.join(nsisDir, file);
    const dest = path.join(outDir, file);
    fs.copyFileSync(src, dest);
    const sizeMb = (fs.statSync(dest).size / (1024 * 1024)).toFixed(2);
    console.log(` ✅ NSIS Setup One-File EXE: ./release-exe/${file} (${sizeMb} MB)`);
  }
}

console.log("\n✨ Done! Your 1-file standalone production executable is ready in:");
console.log(`   📂 ${outDir}\n`);
