import path from 'path';
import { runTests } from 'vscode-test';
import minimist from 'minimist';
import { MOCK_PROJECT_DIRECTORY, MOCK_PROJECT_FILES } from './unit/util/constants';

async function main() {
    const args = minimist(process.argv.slice(2));
    const isInstall = args._.includes('install');

    // The folder containing the Extension Manifest package.json
    // Passed to `--extensionDevelopmentPath`
    const extensionDevelopmentPath = path.resolve(__dirname, '../../');

    const launchArgs = ["--disable-extensions", MOCK_PROJECT_DIRECTORY, ...Object.values(MOCK_PROJECT_FILES)];

    // All test suites (either unit tests or integration tests) should be in subfolders.
    const unitTestsPath = path.resolve(__dirname, './unit/index');
    const installTestsPath = path.resolve(__dirname, './install/index');

    // Run tests using the latest stable release of VSCode
    await runTests({
        version: 'stable',
        launchArgs,
        extensionDevelopmentPath,
        extensionTestsPath: isInstall ? installTestsPath : unitTestsPath,
    });
}

main().catch(err => {
    console.error('Failed to run tests', err);
    process.exit(1);
});
