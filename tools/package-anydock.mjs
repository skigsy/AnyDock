import { execFile } from 'node:child_process'
import { createHash } from 'node:crypto'
import { constants } from 'node:fs'
import { copyFile, lstat, mkdir, mkdtemp, readFile, readdir, realpath, rename, rm, writeFile } from 'node:fs/promises'
import { homedir, tmpdir } from 'node:os'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { promisify } from 'node:util'

const exec = promisify(execFile)
const commands = {
  git: process.env.ANYDOCK_GIT ?? 'git',
  cargo: process.env.ANYDOCK_CARGO ?? 'cargo',
  tar: process.env.ANYDOCK_TAR ?? 'tar',
  python: process.env.ANYDOCK_PYTHON ?? (process.platform === 'win32' ? 'python' : 'python3'),
}
const platform = {
  'win32-x64': 'windows-x64',
  'linux-x64': 'linux-x64',
  'darwin-arm64': 'macos-arm64',
}[`${process.platform}-${process.arch}`]
const product = {
  root: process.env.ANYDOCK_PRODUCT_ROOT
    ? resolve(process.env.ANYDOCK_PRODUCT_ROOT)
    : fileURLToPath(new URL('../', import.meta.url)),
  files: new Set([
    '.gitattributes', '.gitignore', 'AGENTS.md', 'CLAUDE.md', 'README.md', 'USAGE.md',
    'LICENSE', 'Cargo.toml', 'Cargo.lock', 'THIRD_PARTY_NOTICES.md', 'anydock.version',
    '.cursor/rules/anydock.mdc', '.github/copilot-instructions.md', '.gemini/settings.json',
    '.github/workflows/ci.yml', '.github/workflows/release.yml',
    'docs/example.md', 'docs/assets/anydock-banner.svg', 'docs/assets/project-layout.svg',
    'docs/assets/workspace-handoff.svg',
    'tools/package-anydock.mjs', 'tools/assemble-release.mjs',
  ]),
}

const required = ['AGENTS.md', 'README.md', 'Cargo.toml', 'Cargo.lock', 'anydock.version', 'src/main.rs']
const sha = bytes => createHash('sha256').update(bytes).digest('hex')
const digest = async path => sha(await readFile(path))
const run = async (file, args, cwd, env = process.env) => (await exec(file, args, {
  cwd, env, windowsHide: true, timeout: 300_000, maxBuffer: 8 * 1024 * 1024, encoding: 'buffer',
})).stdout
const string = async (...args) => (await run(...args)).toString('utf8').trim()
const exists = async path => lstat(path).then(() => true, error => {
  if (error.code === 'ENOENT') return false
  throw error
})

async function list(root, prefix = '') {
  const files = []
  for (const item of await readdir(root, { withFileTypes: true })) {
    const path = prefix + item.name
    if (item.isDirectory()) files.push(...await list(join(root, item.name), path + '/'))
    else if (item.isFile()) files.push(path)
    else throw Error('Unsupported archive member ' + path)
  }
  return files.sort()
}

async function verifyFiles(root, expected) {
  const names = await list(root)
  if (JSON.stringify(names) !== JSON.stringify(expected.map(file => file.path))) {
    throw Error('Archive members differ from the expected file list')
  }
  for (const file of expected) {
    if (await digest(join(root, file.path)) !== file.sha256) {
      throw Error('Archive bytes differ from the expected bytes for ' + file.path)
    }
  }
}

async function inspectArchive(archive, folder, prefix, expected) {
  await mkdir(folder)
  if (archive.endsWith('.zip')) {
    await run(commands.python, ['-m', 'zipfile', '-e', archive, folder], folder)
  } else {
    await run(commands.tar, ['-xf', archive, '-C', folder], folder)
  }
  const members = await readdir(folder)
  if (members.length !== 1 || members[0] !== prefix) {
    throw Error('Archive must contain exactly one ' + prefix + ' folder')
  }
  const root = join(folder, prefix)
  await verifyFiles(root, expected)
  return root
}

async function cleanup(path, parent, prefix) {
  const resolved = await realpath(path)
  if (resolved !== path || dirname(resolved) !== parent || !basename(resolved).startsWith(prefix)) {
    throw Error('Unexpected packaging scratch path ' + resolved)
  }
  await rm(resolved, { recursive: true, force: true })
  if (await exists(path)) throw Error('Packaging scratch directory still exists')
}

function checkLeak(bytes, roots) {
  const text = [
    bytes.toString('latin1'),
    bytes.toString('utf16le'),
    bytes.subarray(1).toString('utf16le'),
  ].map(value => value.toLowerCase())
  for (const root of roots) {
    const variants = [root, root.replaceAll('\\', '/'), root.replaceAll('\\', '\\\\')]
    if (variants.some(path => text.some(value => value.includes(path.toLowerCase())))) {
      throw Error('Packaged executable contains a local build path')
    }
  }
}

const [directory, revision = 'HEAD', ...extra] = process.argv.slice(2)
if (!directory || !platform || extra.length) {
  throw Error('Usage on Windows x64, Linux x64, or macOS ARM64: node package-anydock.mjs OUTPUT_DIRECTORY [REVISION]')
}
const repo = await realpath(product.root)
const git = (...args) => string(commands.git, args, repo)
const commit = await git('rev-parse', '--verify', '--end-of-options', revision + '^{commit}')
const tree = await git('rev-parse', commit + '^{tree}')
const cargoManifest = await git('show', commit + ':Cargo.toml')
const version = cargoManifest.match(/^version = "([0-9]+\.[0-9]+\.[0-9]+)"$/m)?.[1]
if (!version || !/^name = "anydock"$/m.test(cargoManifest)) throw Error('Invalid AnyDock Cargo package')
const entries = (await run(commands.git, ['ls-tree', '-rz', commit], repo)).toString('utf8')
  .split('\0').filter(Boolean).map(line => {
    const match = line.match(/^(?:100644|100755) blob [a-f0-9]+\t([a-zA-Z0-9_.\/-]+)$/)
    const path = match?.[1]
    if (!path || !(product.files.has(path) || /^(src|tests)\/[a-z0-9_/.-]+\.rs$/.test(path))) {
      throw Error('Unexpected release tree entry ' + line)
    }
    return path
  }).sort()
for (const file of required) if (!entries.includes(file)) throw Error('Missing product file ' + file)
const license = cargoManifest.match(/^license = "(MIT|Apache-2\.0)"$/m)?.[1] ?? null
const releaseTag = process.env.ANYDOCK_RELEASE_TAG
if (releaseTag && (!license || !entries.includes('LICENSE'))) {
  throw Error('BLOCKER: an owner-selected MIT or Apache-2.0 license is required for publication')
}
if (releaseTag && (releaseTag !== 'v' + version ||
    await git('rev-parse', '--verify', '--end-of-options', 'refs/tags/' + releaseTag + '^{commit}') !== commit)) {
  throw Error('Release tag does not match the exact versioned commit')
}
const sourceFiles = await Promise.all(entries.map(async path => {
  const bytes = await run(commands.git, ['cat-file', 'blob', commit + ':' + path], repo)
  return { path, sha256: sha(bytes) }
}))

const absoluteOutput = resolve(directory)
const outputParent = await realpath(dirname(absoluteOutput))
const output = join(outputParent, basename(absoluteOutput))
if (await exists(output)) throw Error('Package destination already exists: ' + output)
const tempParent = await realpath(tmpdir())
const scratchPrefix = 'anydock-package-'
const scratch = await mkdtemp(join(tempParent, scratchPrefix))
let pending

try {
  const prefix = 'AnyDock-' + version
  const sourceZip = join(scratch, prefix + '-source.zip')
  const binaryArchive = join(scratch, prefix + '-' + platform + (process.platform === 'win32' ? '.zip' : '.tar.gz'))
  const extraction = join(scratch, 'Build files with spaces')
  await git('-c', 'core.autocrlf=false', 'archive', '--format=zip', '--prefix=' + prefix + '/',
    '--output=' + sourceZip, commit)
  const source = await inspectArchive(sourceZip, extraction, prefix, sourceFiles)
  const home = await realpath(homedir())
  const cargoHome = await realpath(process.env.CARGO_HOME ?? join(home, '.cargo'))
  const rustupHome = await realpath(process.env.RUSTUP_HOME ?? join(home, '.rustup')).catch(error => {
    if (error.code !== 'ENOENT') throw error
    return null
  })
  const roots = [...new Set([home, cargoHome, rustupHome, repo, scratch].filter(Boolean))]
  const remaps = roots.map((root, index) => `--remap-path-prefix=${root}=/build/${index}`)
  const target = join(scratch, 'target')
  await run(commands.cargo, ['build', '--locked', '--release'], source, {
    ...process.env,
    CARGO_TARGET_DIR: target, CARGO_INCREMENTAL: '0', CARGO_ENCODED_RUSTFLAGS: remaps.join('\x1f'),
  })
  const executable = process.platform === 'win32' ? 'anydock.exe' : 'anydock'
  const bin = 'bin/' + executable
  const built = join(target, 'release', executable)
  const bytes = await readFile(built)
  checkLeak(bytes, roots)
  await mkdir(join(source, 'bin'))
  await copyFile(built, join(source, bin), constants.COPYFILE_EXCL)
  const files = [...sourceFiles, { path: bin, sha256: sha(bytes) }]
    .sort((a, b) => a.path < b.path ? -1 : a.path > b.path ? 1 : 0)
  if (binaryArchive.endsWith('.zip')) {
    await run(commands.tar, ['-a', '-cf', binaryArchive, prefix], extraction)
  } else {
    await run(commands.tar, ['-czf', binaryArchive, prefix], extraction)
  }
  const extracted = await inspectArchive(binaryArchive, join(scratch, 'Check files with spaces'), prefix, files)
  const blank = await string(join(extracted, bin), ['check'], extracted)
  if (!blank.includes('0 runs, 0 workflows')) throw Error('Shipped workspace is not empty')
  const rust = await string(process.env.RUSTC ?? 'rustc', ['--version'], source)
  const archives = await Promise.all([sourceZip, binaryArchive].map(async path => ({
    name: basename(path), sha256: await digest(path),
  })))

  pending = await mkdtemp(join(outputParent, 'anydock-output-'))
  for (const path of [sourceZip, binaryArchive]) {
    await copyFile(path, join(pending, basename(path)), constants.COPYFILE_EXCL)
  }
  await writeFile(join(pending, 'MANIFEST.json'), JSON.stringify({
    commit, tree, version, license, platform, rust, root: prefix, archives, files,
  }, null, 2) + '\n', { flag: 'wx' })
  await writeFile(join(pending, 'SHA256SUMS.txt'),
    archives.map(file => `${file.sha256}  ${file.name}\n`).join(''), { flag: 'wx' })
  if (await exists(output)) throw Error('Package destination was created during build: ' + output)
  await rename(pending, output)
  pending = null
  console.log(output)
} finally {
  try {
    if (pending) await cleanup(pending, outputParent, 'anydock-output-')
  } finally {
    await cleanup(scratch, tempParent, scratchPrefix)
  }
}
