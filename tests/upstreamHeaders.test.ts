import { parseUpstreamHeadersInput } from '../src/utils/upstreamHeaders';

describe('parseUpstreamHeadersInput', () => {
  it('should return undefined for empty input', () => {
    expect(parseUpstreamHeadersInput('')).toBeUndefined();
    expect(parseUpstreamHeadersInput('   ')).toBeUndefined();
  });

  it('should parse a valid headers JSON object', () => {
    expect(
      parseUpstreamHeadersInput('{"HTTP-Referer":"https://example.com","X-Title":"Claude Adapter"}')
    ).toEqual({
      'HTTP-Referer': 'https://example.com',
      'X-Title': 'Claude Adapter',
    });
  });

  it('should reject invalid JSON', () => {
    expect(() => parseUpstreamHeadersInput('{bad json')).toThrow('Please enter a valid JSON object');
  });

  it('should reject arrays', () => {
    expect(() => parseUpstreamHeadersInput('[]')).toThrow('Headers must be a JSON object');
  });

  it('should reject empty header names', () => {
    expect(() => parseUpstreamHeadersInput('{"":"value"}')).toThrow(
      'Header names must be non-empty strings'
    );
  });

  it('should reject non-string header values', () => {
    expect(() => parseUpstreamHeadersInput('{"X-Title":123}')).toThrow(
      'Header "X-Title" value must be a non-empty string'
    );
  });

  it('should reject empty header values', () => {
    expect(() => parseUpstreamHeadersInput('{"X-Title":"   "}')).toThrow(
      'Header "X-Title" value must be a non-empty string'
    );
  });

  it('should reject reserved headers case-insensitively', () => {
    expect(() => parseUpstreamHeadersInput('{"Authorization":"Bearer token"}')).toThrow(
      'Header "Authorization" is reserved and cannot be configured'
    );
    expect(() => parseUpstreamHeadersInput('{"CONTENT-TYPE":"application/json"}')).toThrow(
      'Header "CONTENT-TYPE" is reserved and cannot be configured'
    );
  });
});
