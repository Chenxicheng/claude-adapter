#!/usr/bin/env node
import { chmodSync, existsSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, extname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const JavaScriptObfuscator = require('javascript-obfuscator');

const rootDir = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const distDir = join(rootDir, 'dist');
const cliEntry = join(distDir, 'cli.js');
const sourceMapCommentPattern = /^\s*\/\/# sourceMappingURL=.*$/gm;
const shebangPattern = /^#!.*(?:\r?\n|$)/;

const obfuscationOptions = {
  compact: true,
  controlFlowFlattening: false,
  deadCodeInjection: false,
  debugProtection: false,
  disableConsoleOutput: false,
  identifierNamesGenerator: 'hexadecimal',
  renameGlobals: false,
  renameProperties: false,
  selfDefending: false,
  simplify: true,
  sourceMap: false,
  stringArray: true,
  stringArrayEncoding: ['base64'],
  stringArrayThreshold: 0.35,
  target: 'node',
  transformObjectKeys: false,
  unicodeEscapeSequence: false,
};

function walkFiles(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const filePath = join(dir, entry.name);
    return entry.isDirectory() ? walkFiles(filePath) : [filePath];
  });
}

function stripSourceMapComment(source) {
  return source.replace(sourceMapCommentPattern, '').trimEnd() + '\n';
}

function obfuscateFile(filePath) {
  const originalSource = readFileSync(filePath, 'utf8');
  const shebang = originalSource.match(shebangPattern)?.[0].trimEnd();
  const sourceWithoutShebang = originalSource.replace(shebangPattern, '');
  const sourceWithoutMap = stripSourceMapComment(sourceWithoutShebang);
  const result = JavaScriptObfuscator.obfuscate(sourceWithoutMap, obfuscationOptions);
  const obfuscatedSource = result.getObfuscatedCode();
  const output = shebang ? `${shebang}\n${obfuscatedSource}\n` : `${obfuscatedSource}\n`;

  writeFileSync(filePath, output, 'utf8');
}

function stripDeclarationMapReference(filePath) {
  const source = readFileSync(filePath, 'utf8');
  writeFileSync(filePath, stripSourceMapComment(source), 'utf8');
}

if (!existsSync(distDir)) {
  throw new Error(`Expected build output directory not found: ${distDir}`);
}

const files = walkFiles(distDir);

for (const filePath of files) {
  if (filePath.endsWith('.map')) {
    rmSync(filePath, { force: true });
  }
}

for (const filePath of files) {
  if (extname(filePath) === '.js') {
    obfuscateFile(filePath);
  } else if (filePath.endsWith('.d.ts')) {
    stripDeclarationMapReference(filePath);
  }
}

if (existsSync(cliEntry)) {
  chmodSync(cliEntry, 0o755);
}

console.log(`Obfuscated JavaScript files in ${distDir}`);
