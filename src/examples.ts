export interface Example {
  id: string;
  title: { en: string; zh: string };
  code: string;
}

export const EXAMPLES: Example[] = [
  {
    id: "hello",
    title: { en: "Hello, measurement", zh: "你好，测量" },
    code: `/* ── Hello, measurement ──────────────────────────────
   Build, then Run. The simulated measurement starts at
   t = 0 ms and runs on virtual time.                     */

variables
{
  msTimer tick;
  int      counter = 0;
  char     greeting[32] = "Hello from CAPL on WebAssembly";
}

on preStart
{
  write("preStart: loading node configuration");
}

on start
{
  write("%s", greeting);
  setTimer(tick, 100);
}

on timer tick
{
  counter++;
  write("tick #%d at %d ms", counter, timeNow());

  if (counter < 5)
    setTimer(tick, 100);
  else
    write("done after %d ticks", counter);
}

on key 'r'
{
  counter = 0;
  write("counter reset by key 'r'");
  setTimer(tick, 50);
}
`,
  },
  {
    id: "bus",
    title: { en: "Cyclic frame + receive", zh: "周期报文与接收" },
    code: `/* ── Cyclic transmitter and receiver ─────────────────
   output() puts the frame on the virtual bus, which is
   then dispatched back into 'on message' handlers.      */

variables
{
  message 0x100 engineMsg;
  msTimer       txTimer;
  long          txCount   = 0;
  word          rpm       = 800;
}

on start
{
  engineMsg.dlc = 8;
  write("transmitting 0x100 every 200 ms");
  setTimerCyclic(txTimer, 200);
}

on timer txTimer
{
  txCount++;
  rpm = rpm + 125;
  if (rpm > 6000) rpm = 800;

  engineMsg.byte(0) = (rpm >> 8) & 0xFF;
  engineMsg.byte(1) = rpm & 0xFF;
  engineMsg.byte(2) = txCount & 0xFF;
  output(engineMsg);
}

on message 0x100
{
  word v;
  v = (this.byte(0) << 8) | this.byte(1);
  write("0x%03X dlc=%d  rpm=%5d  seq=%d", this.id, this.dlc, v, this.byte(2));
}

on message 0x200
{
  write("gateway frame 0x200 received, first byte = 0x%02X", this.byte(0));
}
`,
  },
  {
    id: "gateway",
    title: { en: "Gateway + functions", zh: "网关与函数" },
    code: `/* ── Signal gateway with user functions and arrays ───
   0x100 arrives, is filtered through a moving average
   and re-transmitted as 0x200.                          */

variables
{
  message 0x200 outMsg;
  long  window[8];
  int   widx  = 0;
  long  total = 0;
}

long average(long values[], long n)
{
  long i;
  long sum = 0;

  for (i = 0; i < n; i++)
    sum += values[i];

  return sum / n;
}

void pushSample(long v)
{
  window[widx] = v;
  widx = (widx + 1) % elCount(window);
  total++;
}

on message 0x100
{
  long raw;
  long avg;

  raw = (this.byte(0) << 8) | this.byte(1);
  pushSample(raw);
  avg = average(window, elCount(window));

  outMsg.dlc = 4;
  outMsg.byte(0) = (avg >> 8) & 0xFF;
  outMsg.byte(1) = avg & 0xFF;
  outMsg.byte(2) = widx;
  output(outMsg);

  write("sample %2d: raw=%5d  avg=%5d", total, raw, avg);
}

on key 's'
{
  write("--- window ---");
  pushSample(random(4096));
  write("avg now %d over %d slots", average(window, elCount(window)), elCount(window));
}
`,
  },
  {
    id: "algo",
    title: { en: "Algorithms & control flow", zh: "算法与控制流" },
    code: `/* ── Everything the code generator can do ────────────
   recursion, loops, switch/fallthrough, 64-bit maths,
   doubles and arrays — all compiled to WebAssembly.     */

variables
{
  long data[10] = { 42, 7, 19, 3, 88, 23, 5, 61, 14, 30 };
}

long fib(long n)
{
  if (n < 2) return n;
  return fib(n - 1) + fib(n - 2);
}

void sortData()
{
  long i, j, tmp;

  for (i = 0; i < elCount(data) - 1; i++)
  {
    for (j = 0; j < elCount(data) - 1 - i; j++)
    {
      if (data[j] > data[j + 1])
      {
        tmp = data[j];
        data[j] = data[j + 1];
        data[j + 1] = tmp;
      }
    }
  }
}

void describe(long n)
{
  switch (n % 3)
  {
    case 0:
      write("  %d is a multiple of three", n);
      break;
    case 1:
    case 2:
      write("  %d is not", n);
      break;
    default:
      write("  unreachable");
  }
}

on start
{
  long i;
  int64 big = 1;
  double x = 2.0;

  write("fib(20) = %d", fib(20));

  sortData();
  write("sorted:");
  for (i = 0; i < elCount(data); i++)
    write("  data[%d] = %d", i, data[i]);

  for (i = 0; i < 20; i++) big = big * 3;
  write("3^20 = %d  (int64)", big);

  x = sqrt(2.0);
  write("sqrt(2) = %.9f   pow(x,8) = %.4f", x, pow(x, 8.0));

  i = 0;
  while (i < 3) { describe(i * 7); i++; }
}
`,
  },
  {
    id: "md5",
    title: { en: "MD5 hash (CAPL implementation)", zh: "MD5 哈希（CAPL 实现）" },
    code: `/* ── MD5 for the single-block message "abc" ─────────
   A complete message-digest implementation using arrays,
   functions, loops, conditionals and bitwise operations. */

variables
{
  dword state[4];
  dword block[16] = { 0x80636261, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 24, 0 };
  dword result[4];
  dword constants[64] = {
    0xD76AA478, 0xE8C7B756, 0x242070DB, 0xC1BDCEEE, 0xF57C0FAF, 0x4787C62A, 0xA8304613, 0xFD469501,
    0x698098D8, 0x8B44F7AF, 0xFFFF5BB1, 0x895CD7BE, 0x6B901122, 0xFD987193, 0xA679438E, 0x49B40821,
    0xF61E2562, 0xC040B340, 0x265E5A51, 0xE9B6C7AA, 0xD62F105D, 0x02441453, 0xD8A1E681, 0xE7D3FBC8,
    0x21E1CDE6, 0xC33707D6, 0xF4D50D87, 0x455A14ED, 0xA9E3E905, 0xFCEFA3F8, 0x676F02D9, 0x8D2A4C8A,
    0xFFFA3942, 0x8771F681, 0x6D9D6122, 0xFDE5380C, 0xA4BEEA44, 0x4BDECFA9, 0xF6BB4B60, 0xBEBFBC70,
    0x289B7EC6, 0xEAA127FA, 0xD4EF3085, 0x04881D05, 0xD9D4D039, 0xE6DB99E5, 0x1FA27CF8, 0xC4AC5665,
    0xF4292244, 0x432AFF97, 0xAB9423A7, 0xFC93A039, 0x655B59C3, 0x8F0CCC92, 0xFFEFF47D, 0x85845DD1,
    0x6FA87E4F, 0xFE2CE6E0, 0xA3014314, 0x4E0811A1, 0xF7537E82, 0xBD3AF235, 0x2AD7D2BB, 0xEB86D391
  };
  long shifts[64] = { 7,12,17,22,7,12,17,22,7,12,17,22,7,12,17,22, 5,9,14,20,5,9,14,20,5,9,14,20,5,9,14,20, 4,11,16,23,4,11,16,23,4,11,16,23,4,11,16,23, 6,10,15,21,6,10,15,21,6,10,15,21,6,10,15,21 };
}

dword rol(dword value, dword amount)
{
  return (value << amount) | (value >> (32 - amount));
}

int calculateMd5()
{
  long n, index, shift;
  dword a, b, c, d, temp, mix;
  state[0] = 0x67452301; state[1] = 0xEFCDAB89;
  state[2] = 0x98BADCFE; state[3] = 0x10325476;
  a = state[0]; b = state[1]; c = state[2]; d = state[3];
  for (n = 0; n < 64; n++) {
    if (n < 16) { mix = (b & c) | ((~b) & d); index = n; }
    else if (n < 32) { mix = (b & d) | (c & (~d)); index = (5 * n + 1) % 16; }
    else if (n < 48) { mix = b ^ c ^ d; index = (3 * n + 5) % 16; }
    else { mix = c ^ (b | (~d)); index = (7 * n) % 16; }
    temp = d; d = c; c = b; shift = shifts[n];
    b = b + rol(a + mix + block[index] + constants[n], shift); a = temp;
  }
  result[0] = state[0] + a; result[1] = state[1] + b;
  result[2] = state[2] + c; result[3] = state[3] + d;
  return (result[0] == 0x98500190 && result[1] == 0xB04FD23C && result[2] == 0x7D3F96D6 && result[3] == 0x727FE128);
}

on start
{
  int match;
  match = calculateMd5();
  write("MD5(\\\"abc\\\") calculated words: %x%x%x%x", result[0], result[1], result[2], result[3]);
  write("MD5(\\\"abc\\\") expected:         900150983cd24fb0d6963f7d28e17f72");
  if (match) write("MD5 check: MATCH"); else write("MD5 check: MISMATCH");
}
`,
  },
  {
    id: "types",
    title: { en: "Enums, structs, matrices", zh: "枚举、结构与多维数组" },
    code: `/* ── enum, struct and multi-dimensional arrays ───────
   Struct layouts (member offsets and padding) are
   computed by capl-sema and carried inside the type,
   so the backends need no symbol table at all.        */

enum Mode { OFF, RUNNING = 4, FAULT, MAINTENANCE }

struct Sensor
{
  char     name[8];
  int      raw;
  long     scaled;
  double   factor;
  byte     status;
}

variables
{
  /* anonymous struct + array of structs + a 3x4 matrix */
  struct
  {
    long   matrix[3][4];
    struct Sensor bank[2];
  } bench;
}

/* aggregates cannot be passed by value yet, so scalar parameters it is */
void showSensor(int raw, long scaled, byte status)
{
  write("  raw=%5d scaled=%5d status=%d", raw, scaled, status);
}

long traceSum()
{
  long sum = 0;
  int  r, c;

  for (r = 0; r < 3; r++)
    for (c = 0; c < 4; c++)
      sum += bench.matrix[r][c];
  return sum;
}

on start
{
  enum Mode current = RUNNING;   /* local enum declaration */
  int i;

  bench.matrix[0][0] = 1;
  bench.matrix[1][2] = 5;
  bench.matrix[2][3] = 9;

  bench.bank[0].raw     = 1023;
  bench.bank[0].scaled  = 2046;
  bench.bank[0].factor  = 2.0;
  bench.bank[0].status  = 0x01;
  bench.bank[0].name[0] = 'A';
  bench.bank[0].name[1] = 0;

  bench.bank[1].raw    = 4095;
  bench.bank[1].scaled = 8190;
  bench.bank[1].status = 0x02;

  write("mode = %d (RUNNING == %d)", current, RUNNING);
  write("matrix sum = %d", traceSum());

  for (i = 0; i < 2; i++)
  {
    write("sensor %d:", i);
    showSensor(bench.bank[i].raw, bench.bank[i].scaled, bench.bank[i].status);
  }

  switch (FAULT)
  {
    case OFF:        write("OFF is %d", OFF);        break;
    case RUNNING:    write("RUNNING is %d", RUNNING); break;
    case FAULT:      write("FAULT is %d", FAULT);     break;
    default:         write("MAINTENANCE is %d", MAINTENANCE);
  }
}

on sysvar Demo::Rpm
{
  write("sysvar Demo::Rpm changed to %d", this);
}

on errorFrame
{
  write("error frame on the bus!");
}
`,
  },
  {
    id: "state",
    title: { en: "Key-driven state machine", zh: "按键状态机" },
    code: `/* ── A small state machine driven by key events ──────
   Press the keys in the simulation bar below.           */

variables
{
  const int IDLE    = 0;
  const int RUNNING = 1;
  const int FAULT   = 2;

  int     state = IDLE;
  msTimer watchdog;
  long    ticks = 0;
}

void enter(int s)
{
  state = s;
  switch (s)
  {
    case 0: write("[state] IDLE");    cancelTimer(watchdog); break;
    case 1: write("[state] RUNNING"); setTimerCyclic(watchdog, 250); break;
    case 2: write("[state] FAULT");   cancelTimer(watchdog); break;
  }
}

on start
{
  write("keys:  g = go, s = stop, f = fault, c = clear");
  enter(IDLE);
}

on timer watchdog
{
  ticks++;
  write("watchdog %d (state=%d)", ticks, state);
  if (ticks % 8 == 0)
  {
    write("watchdog timeout!");
    enter(FAULT);
  }
}

on key 'g' { if (state != FAULT) enter(RUNNING); else write("clear the fault first"); }
on key 's' { enter(IDLE); }
on key 'f' { enter(FAULT); }
on key 'c' { ticks = 0; enter(IDLE); }
`,
  },
];
