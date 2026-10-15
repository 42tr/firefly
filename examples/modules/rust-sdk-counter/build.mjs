import { execFileSync } from 'node:child_process'
import { access, chmod, copyFile, mkdir, readFile, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { build } from 'vite'

const example = fileURLToPath(new URL('.', import.meta.url))
const root = fileURLToPath(new URL('../../../', import.meta.url))
const args = process.argv.slice(2)
if (args.length > 1 || (args.length === 1 && args[0] !== '--release')) {
  throw new Error('Usage: node examples/modules/rust-sdk-counter/build.mjs [--release]')
}
const profile = args[0] === '--release' ? 'release' : 'debug'
const metadata = JSON.parse(execFileSync('cargo', ['metadata', '--locked', '--no-deps', '--format-version', '1'], {
  cwd: root, encoding: 'utf8',
}))
const binary = join(metadata.target_directory, profile, 'firefly-sdk-counter-example')
try { await access(binary) } catch {
  throw new Error(`Build the binary first: cargo build --locked -p firefly-sdk-counter-example${profile === 'release' ? ' --release' : ''}`)
}
const bundle = await build({
  configFile: false,
  root: example,
  publicDir: false,
  resolve: { alias: { '@firefly-module-sdk': join(root, 'crates/module-sdk/ui') } },
  build: {
    write: false,
    target: 'es2022',
    lib: { entry: join(example, 'ui/main.js'), formats: ['es'] },
    rollupOptions: { output: { inlineDynamicImports: true } },
  },
})
const output = (Array.isArray(bundle) ? bundle[0] : bundle).output
const scripts = output.filter(item => item.type === 'chunk')
if (scripts.length !== 1 || scripts[0].imports.length || scripts[0].dynamicImports.length) {
  throw new Error('Module UI must be a single self-contained script')
}
const css = output.filter(item => item.type === 'asset' && item.fileName.endsWith('.css'))
  .map(item => Buffer.from(item.source).toString('utf8')).join('\n')
const template = await readFile(join(example, 'ui/index.html'), 'utf8')
const html = template.replace('__MODULE_STYLE__', () => css)
  .replace('__MODULE_SCRIPT__', () => scripts[0].code.replace(/<\/script/gi, '<\\/script'))
const destination = join(root, 'output/modules/rust-sdk-counter')
await mkdir(destination, { recursive: true })
for (const file of ['module.toml', 'config.schema.json']) {
  await copyFile(join(example, file), join(destination, file))
}
await copyFile(binary, join(destination, 'firefly-sdk-counter-example'))
await chmod(join(destination, 'firefly-sdk-counter-example'), 0o755)
await writeFile(join(destination, 'ui.html'), html)
console.log(`Module package: ${destination}`)
