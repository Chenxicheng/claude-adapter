#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, basename, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const rootDir = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const releaseDir = join(rootDir, 'release');
const stagingDir = join(releaseDir, 'pack-staging');
const packageJson = JSON.parse(readFileSync(join(rootDir, 'package.json'), 'utf8'));
const version = packageJson.version;

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: rootDir,
    stdio: 'inherit',
    shell: process.platform === 'win32',
    ...options,
  });

  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}

function resetBuildDirs() {
  rmSync(releaseDir, { recursive: true, force: true });
  rmSync(join(rootDir, 'dist'), { recursive: true, force: true });
  mkdirSync(stagingDir, { recursive: true });
}

function createStagingPackage() {
  cpSync(join(rootDir, 'dist'), join(stagingDir, 'dist'), { recursive: true });
  cpSync(join(rootDir, 'LICENSE'), join(stagingDir, 'LICENSE'));

  const stagedPackageJson = {
    name: packageJson.name,
    version,
    description: packageJson.description,
    main: packageJson.main,
    types: packageJson.types,
    bin: packageJson.bin,
    keywords: packageJson.keywords,
    author: packageJson.author,
    license: packageJson.license,
    repository: packageJson.repository,
    bugs: packageJson.bugs,
    homepage: packageJson.homepage,
    dependencies: packageJson.dependencies,
    engines: packageJson.engines,
    files: ['dist', 'LICENSE'],
  };

  writeFileSync(join(stagingDir, 'package.json'), `${JSON.stringify(stagedPackageJson, null, 2)}\n`);
}

function createPackAsset() {
  run('npm', ['pack', '--pack-destination', releaseDir], { cwd: stagingDir });
  const tarball = join(releaseDir, `${packageJson.name}-${version}.tgz`);

  if (!existsSync(tarball)) {
    throw new Error(`Expected package tarball not found: ${tarball}`);
  }

  validatePackAsset(tarball);
}

function validatePackAsset(tarball) {
  const list = spawnSync('tar', ['-tzf', tarball], {
    cwd: rootDir,
    encoding: 'utf8',
  });

  if (list.status !== 0) {
    process.stderr.write(list.stderr);
    process.exit(list.status ?? 1);
  }

  const forbidden = list.stdout
    .split('\n')
    .filter(Boolean)
    .filter((entry) => {
      const fileName = basename(entry).toLowerCase();
      return (
        fileName === 'readme' ||
        fileName.startsWith('readme.') ||
        fileName.endsWith('.md') ||
        entry.includes('/test/') ||
        entry.includes('/tests/') ||
        fileName.includes('.test.')
      );
    });

  if (forbidden.length > 0) {
    throw new Error(`Pack asset contains forbidden files:\n${forbidden.join('\n')}`);
  }
}

function createSourceArchive() {
  const archivePath = join(releaseDir, `${packageJson.name}-v${version}-source.zip`);
  run('git', ['archive', '--format=zip', '--output', archivePath, 'HEAD']);
}

resetBuildDirs();
run('npm', ['run', 'build']);
createStagingPackage();
createPackAsset();
createSourceArchive();

console.log(`Release assets written to ${releaseDir}`);
