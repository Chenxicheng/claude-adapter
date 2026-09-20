const PORTABLE_ENV_NAME = /^[A-Za-z_][A-Za-z0-9_]*$/;

export function parseApiKeyEnvInput(
  input: string,
  environment: NodeJS.ProcessEnv = process.env
): string {
  const name = input.trim();
  if (!PORTABLE_ENV_NAME.test(name)) {
    throw new Error(
      'Environment variable name must match [A-Za-z_][A-Za-z0-9_]*'
    );
  }
  const value = environment[name];
  if (value === undefined) {
    throw new Error(`Environment variable "${name}" is not set`);
  }
  if (value.length === 0) {
    throw new Error(`Environment variable "${name}" is empty`);
  }
  return name;
}
