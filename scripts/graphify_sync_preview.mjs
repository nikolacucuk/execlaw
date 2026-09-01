#!/usr/bin/env node
// Build a small graph preview payload for the SPA from graphify-out/graph.json.

import fs from "node:fs";
import path from "node:path";

const root = process.cwd();
const src = path.join(root, "graphify-out", "graph.json");
const out = path.join(root, "web", "src", "generated", "graphifyPreview.json");

if (!fs.existsSync(src)) {
  console.error(`missing graph file: ${src}`);
  process.exit(2);
}

const graph = JSON.parse(fs.readFileSync(src, "utf8"));
const nodes = Array.isArray(graph.nodes) ? graph.nodes.slice(0, 300) : [];
const keepIds = new Set(nodes.map((n) => n.id));
const edges = Array.isArray(graph.edges)
  ? graph.edges.filter((e) => keepIds.has(e.source) && keepIds.has(e.target)).slice(0, 800)
  : [];

const payload = {
  source_path: "graphify-out/graph.json",
  generated_at: new Date().toISOString(),
  nodes,
  edges,
};

fs.mkdirSync(path.dirname(out), { recursive: true });
fs.writeFileSync(out, JSON.stringify(payload, null, 2));
console.log(`wrote ${out}`);
