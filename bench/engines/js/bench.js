#!/usr/bin/env node
// Benchmark CLI for jsonata-js (reference implementation).
// Usage: node js_bench.js -expr 'Account.Name' -data '{"Account":{"Name":"Firefly"}}' [-n 1000]
//        node js_bench.js -expr 'Account.Name' -datafile data.json [-n 1000]
"use strict";

const fs = require("fs");
const jsonata = require("jsonata");

function parseArgs(argv) {
  const args = { expr: "", data: "{}", datafile: "", n: 1 };
  for (let i = 2; i < argv.length; i++) {
    switch (argv[i]) {
      case "-expr":
        args.expr = argv[++i];
        break;
      case "-data":
        args.data = argv[++i];
        break;
      case "-datafile":
        args.datafile = argv[++i];
        break;
      case "-n":
        args.n = parseInt(argv[++i], 10);
        break;
    }
  }
  return args;
}

async function main() {
  const args = parseArgs(process.argv);

  if (!args.expr) {
    process.stderr.write(
      "usage: node js_bench.js -expr EXPR [-data JSON | -datafile FILE] [-n ITERS]\n"
    );
    process.exit(1);
  }

  let jsonStr = args.data;
  if (args.datafile) {
    jsonStr = fs.readFileSync(args.datafile, "utf8");
  }

  const input = JSON.parse(jsonStr);
  const compiled = jsonata(args.expr);

  let result;
  for (let i = 0; i < args.n; i++) {
    result = await compiled.evaluate(input);
  }

  process.stdout.write(JSON.stringify(result) + "\n");
}

main().catch((err) => {
  process.stderr.write("error: " + err.message + "\n");
  process.exit(1);
});
