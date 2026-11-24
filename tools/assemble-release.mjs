import { execFile } from 'node:child_process'
import { createHash } from 'node:crypto'
import { constants } from 'node:fs'
import { copyFile, mkdir, readFile, writeFile } from 'node:fs/promises'
import { join, resolve } from 'node:path'
import { promisify } from 'node:util'

const [tag, downloaded, destination] = process.argv.slice(2)
if (!destination || !/^v[0-9]+\.[0-9]+\.[0-9]+$/.test(tag)) {
  throw Error('Usage: node tools/assemble-release.mjs TAG DOWNLOADED_ARTIFACTS NEW_OUTPUT_DIRECTORY')
}
const exec = promisify(execFile)
const git = async (...args) => (await exec('git', args)).stdout.trim()
const commit = await git('rev-parse', '--verify', '--end-of-options', 'refs/tags/' + tag + '^{commit}')
const tree = await git('rev-parse', commit + '^{tree}')
const cargo = await git('show', commit + ':Cargo.toml')
const version = tag.slice(1)
const license = cargo.match(/^license = "(MIT|Apache-2\.0)"$/m)?.[1]
const licenseText = await git('show', commit + ':LICENSE').catch(() => '')
if (!license || !licenseText) {
  throw Error('BLOCKER: an owner-selected MIT or Apache-2.0 license is required for publication')
}
if (cargo.match(/^version = "([0-9.]+)"$/m)?.[1] !== version) {
  throw Error('Tag version does not match Cargo.toml')
}

const root = 'AnyDock-' + version
const platforms = ['windows-x64', 'linux-x64', 'macos-arm64']
const binaryPath = platform => platform === 'windows-x64' ? 'bin/anydock.exe' : 'bin/anydock'
const digest = async path => createHash('sha256').update(await readFile(path)).digest('hex')
const hashPattern = /^[a-f0-9]{64}$/
const packages = []
const copies = []
let sources
for (const platform of platforms) {
  const dir = resolve(downloaded, 'package-' + platform)
  const manifest = JSON.parse(await readFile(join(dir, 'MANIFEST.json'), 'utf8'))
  if (manifest.commit !== commit || manifest.tree !== tree || manifest.platform !== platform
    || manifest.root !== root || manifest.version !== version || manifest.license !== license) {
    throw Error('Package provenance mismatch for ' + platform)
  }
  const name = root + '-' + platform + (platform === 'windows-x64' ? '.zip' : '.tar.gz')
  const sourceName = root + '-source.zip'
  if (![name, sourceName].every(file => manifest.archives.some(item => item.name === file))
    || manifest.archives.length !== 2
    || !manifest.files.some(item => item.path === binaryPath(platform))
    || !manifest.files.some(item => item.path === 'LICENSE')
    || manifest.files.some(item => !hashPattern.test(item.sha256))) {
    throw Error('Incomplete package manifest for ' + platform)
  }
  const files = manifest.files.filter(item => item.path !== binaryPath(platform))
  if (sources && JSON.stringify(files) !== JSON.stringify(sources)) {
    throw Error('Source bytes differ across platform manifests')
  }
  sources = files
  for (const archive of manifest.archives) {
    const path = join(dir, archive.name)
    if (await digest(path) !== archive.sha256) throw Error('Archive digest mismatch for ' + archive.name)
    if (archive.name === sourceName && platform !== platforms[0]) continue
    copies.push({ ...archive, path })
    packages.push({
      ...archive,
      platform: archive.name === sourceName ? 'source' : platform,
      ...(archive.name === sourceName ? {} : { rust: manifest.rust }),
      files: archive.name === sourceName ? sources : manifest.files,
    })
  }
}

const output = resolve(destination)
await mkdir(output)
for (const archive of copies) await copyFile(archive.path, join(output, archive.name), constants.COPYFILE_EXCL)
await writeFile(join(output, 'MANIFEST.json'),
  JSON.stringify({ commit, tree, version, license, root, packages }, null, 2) + '\n', { flag: 'wx' })
await writeFile(join(output, 'SHA256SUMS.txt'), copies
  .map(archive => `${archive.sha256}  ${archive.name}\n`).join(''), { flag: 'wx' })
