import _ from "lodash";
import vscode from "vscode";
import { exec_notify } from "../../../setup";
import { SliceOutput } from "../../../types";
import { to_vsc_range } from "../../../vsc_utils";
import { TestSlice } from "../mock_data/slices";
import { MOCK_PROJECT_DIRECTORY } from "./constants";

export declare const TOOLCHAIN: {
    channel: string;
    components: string[];
};

const LIBRARY_PATHS: Partial<Record<NodeJS.Platform, string>> = {
    darwin: "DYLD_LIBRARY_PATH",
    win32: "LIB",
};

type TestSliceResult = {
    test: string;
    expected_selections: vscode.Selection[];
    actual_selections: vscode.Selection[];
};

export const get_slice = async ({ test, file, direction, slice_on }: TestSlice): Promise<string> => {
    const doc = vscode.window.activeTextEditor?.document!;
    const start = doc.offsetAt(new vscode.Position(...slice_on[0]));
    const end = doc.offsetAt(new vscode.Position(...slice_on[1]));
    const flowistry_cmd = `cargo +${TOOLCHAIN.channel} flowistry`;
    const slice_command = `${flowistry_cmd} ${direction}_slice ${file} ${start} ${end}`;

    const rustc_path = await exec_notify(
        `rustup which --toolchain ${TOOLCHAIN.channel} rustc`,
        "Waiting for rustc..."
    );
    const target_info = await exec_notify(
        `${rustc_path} --print target-libdir --print sysroot`,
        "Waiting for rustc..."
    );
    const [target_libdir, sysroot] = target_info.split("\n");
    const library_path = LIBRARY_PATHS[process.platform] || "LD_LIBRARY_PATH";

    const output = await exec_notify(slice_command, test, {
        cwd: MOCK_PROJECT_DIRECTORY,
        [library_path]: target_libdir,
        SYSROOT: sysroot,
        RUST_BACKTRACE: "1",
    });

    return output;
};

const slice = async (
    direction: TestSlice['direction'],
    position: TestSlice['slice_on'],
    filename: string,
): Promise<void> => {
    const file = vscode.Uri.parse(filename);
    await vscode.window.showTextDocument(file);

    const start_position = new vscode.Position(...position[0]);
    const end_position = new vscode.Position(...position[1]);

    vscode.window.activeTextEditor!.selection = new vscode.Selection(
        start_position,
        end_position
    );

    await vscode.commands.executeCommand(`flowistry.${direction}_select`);
};

const merge_ranges = (ranges: vscode.Range[]): vscode.Range[] => {
    const merged_ranges = [ranges[0]];

    ranges.slice(1).forEach((range) => {
        const last_range = merged_ranges[merged_ranges.length - 1];
        const intersection = last_range.intersection(range);

        if (!intersection) {
            merged_ranges.push(range);
        }
        else {
            const union = last_range.union(range);
            merged_ranges[merged_ranges.length - 1] = union;
        }
    });

    return merged_ranges;
};

export const get_slice_selections = async (test_slice: TestSlice): Promise<TestSliceResult> => {
    await slice(test_slice.direction, test_slice.slice_on, test_slice.file);

    const raw_slice_data = await get_slice(test_slice);
    const slice_data: SliceOutput = JSON.parse(raw_slice_data).fields[0];

    const unique_ranges = _.uniqWith(slice_data.ranges, _.isEqual);
    const sorted_ranges = _.sortBy(unique_ranges, (range) => [range.start]);
    const vscode_ranges = sorted_ranges.map((range) => to_vsc_range(range, vscode.window.activeTextEditor?.document!));
    const merged_ranges = merge_ranges(vscode_ranges);
    const expected_selections = merged_ranges.map((range) => new vscode.Selection(range.start, range.end));

    const actual_selections = vscode.window.activeTextEditor?.selections!;

    return {
        ...test_slice,
        expected_selections,
        actual_selections,
    };
};

export const resolve_sequentially = async <T, R>(items: T[], resolver: (arg0: T) => Promise<R>): Promise<R[]> => {
    const results: R[] = [];

    for (const item of items) {
        const result = await resolver(item);
        results.push(result);
    }

    return results;
};
