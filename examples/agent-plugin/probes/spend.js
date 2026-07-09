#!/usr/bin/env node

let input = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk) => (input += chunk));
process.stdin.on("end", () => {
  const request = JSON.parse(input || "{}");
  const fs = require("node:fs");
  const lines = fs.readFileSync(request.file, "utf8").trim().split("\n").filter(Boolean);
  const offset = request.cursor?.line || 0;
  const entries = lines.slice(offset).map(JSON.parse);
  process.stdout.write(JSON.stringify({ entries, cursor: { line: lines.length } }));
});
