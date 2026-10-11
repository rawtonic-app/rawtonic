/** The Rawtonic symbol ("Sensor R", docs/brand) in its on-dark colours: the sensor's 2×2
 * Bayer cell as an R whose blue photosite is the leg. Sizes up to 32 px use the small cut
 * with its wider gaps. */
export default function RawtonicMark({ size = 18 }: { size?: number }) {
  const small = size <= 32;
  return (
    <svg
      width={size}
      height={size}
      viewBox="21 20 216 216"
      aria-hidden
      focusable="false"
      className="rawtonic-mark"
    >
      {small ? (
        <>
          <path fill="#dde4e9" d="M24 24H114V114H24ZM142 24H189A45 45 0 0 1 189 114H142ZM24 142H114V232H24Z" />
          <path fill="#9cb8d0" d="M142 142H187L232 232H187Z" />
        </>
      ) : (
        <>
          <path fill="#dde4e9" d="M24 24H120V120H24ZM136 24H186A48 48 0 0 1 186 120H136ZM24 136H120V232H24Z" />
          <path fill="#9cb8d0" d="M136 136H184L232 232H184Z" />
        </>
      )}
    </svg>
  );
}
