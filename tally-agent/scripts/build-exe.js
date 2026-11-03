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
console.log("[1/4] Building frontend assets (Vite + TypeScript)...");
execSync("npm run build", { cwd: rootDir, stdio: "inherit" });

// 2. Build Tauri release (Bundles Rust + Embedded Frontend into single binary)
console.log("\n[2/4] Compiling Tauri release binary & bundle...");
execSync("npx tauri build", { cwd: rootDir, stdio: "inherit" });

// 3. Extract standalone single EXE to clean output folder
console.log("\n[3/4] Exporting standalone single EXE to ./release-exe/ ...");

const releaseDir = path.join(rootDir, "src-tauri", "target", "release");
const nsisDir = path.join(releaseDir, "bundle", "nsis");
const exportedFiles = [];

// Direct standalone single executable
const directExe = path.join(releaseDir, "fininsight-tally-agent.exe");
if (fs.existsSync(directExe)) {
  const destPath = path.join(outDir, "fininsight-tally-agent.exe");
  fs.copyFileSync(directExe, destPath);
  const sizeMb = (fs.statSync(destPath).size / (1024 * 1024)).toFixed(2);
  console.log(` ✅ Standalone One-File EXE: ./release-exe/fininsight-tally-agent.exe (${sizeMb} MB)`);
  exportedFiles.push(destPath);
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
    exportedFiles.push(dest);
  }
}

// 4. Digital Authenticode Signing Step
console.log("\n[4/4] Authenticode Digital Code Signing Check...");
const thumbprint = process.env.CERT_THUMBPRINT || process.env.TAURI_SIGNING_THUMBPRINT;
const timestampUrl = process.env.TIMESTAMP_SERVER || "http://timestamp.digicert.com";

if (thumbprint) {
  console.log(` 🔏 Signing release executables with thumbprint: ${thumbprint}...`);
  for (const filePath of exportedFiles) {
    try {
      execSync(
        `signtool sign /sha1 "${thumbprint}" /tr "${timestampUrl}" /td sha256 /fd sha256 "${filePath}"`,
        { stdio: "inherit" }
      );
      console.log(`    ✓ Signed: ${path.basename(filePath)}`);
    } catch (signErr) {
      console.error(`    ⚠️ Failed to sign ${path.basename(filePath)} with signtool:`, signErr.message);
    }
  }
} else {
  console.log(" ℹ️  No CERT_THUMBPRINT environment variable provided. Executables left unsigned.");
  console.log("    To eliminate Windows SmartScreen warnings in production, set $env:CERT_THUMBPRINT=\"<THUMBPRINT>\"");
}

console.log("\n✨ Done! Your production executable is ready in:");
console.log(`   📂 ${outDir}\n`);
