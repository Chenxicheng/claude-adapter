// Shared file storage utilities for daily JSON files
import { existsSync, mkdirSync, appendFileSync } from 'fs';
import { mkdir, appendFile } from 'fs/promises';
import { homedir } from 'os';
import { join } from 'path';

// Base directory for all claude-adapter data
const BASE_DIR = join(homedir(), '.claude-adapter');
let jsonLineWriteQueue = Promise.resolve();

/**
 * Get today's date as YYYY-MM-DD
 */
export function getTodayDateString(): string {
    return new Date().toISOString().split('T')[0];
}

/**
 * Ensure a directory exists, creating it if necessary
 */
export function ensureDirExists(dirPath: string): void {
    if (!existsSync(dirPath)) {
        mkdirSync(dirPath, { recursive: true });
    }
}

/**
 * Get the base storage directory
 */
export function getBaseDir(): string {
    return BASE_DIR;
}

/**
 * Append a JSON record to a file (one JSON object per line)
 * This is atomic on most filesystems and avoids race conditions
 */
export function appendJsonLine(filePath: string, record: object): void {
    const line = JSON.stringify(record) + '\n';
    appendFileSync(filePath, line, 'utf-8');
}

async function writeJsonLine(dirPath: string, filePath: string, line: string): Promise<void> {
    await mkdir(dirPath, { recursive: true });
    await appendFile(filePath, line, 'utf-8');
}

/**
 * Queue a JSONL write so request handling is not blocked by filesystem I/O.
 */
export function enqueueJsonLineWrite(dirPath: string, filePath: string, record: object): void {
    const line = JSON.stringify(record) + '\n';

    jsonLineWriteQueue = jsonLineWriteQueue
        .then(() => writeJsonLine(dirPath, filePath, line))
        .catch(() => {
            // Fail silently - JSONL records are observability data, not request-critical state
        });
}

/**
 * Wait for queued JSONL writes. Intended for tests and graceful verification.
 */
export function flushJsonLineWrites(): Promise<void> {
    return jsonLineWriteQueue;
}
