// 时区换算：改时区时，把已经设好的时刻（就寝、免打扰、每日提醒）换算到新时区，让它们还是同一个真实时刻

/** zone 此刻相对 UTC 的偏移（分钟，东八区是 480）；zone 为 null 时用电脑的时区 */
export function zoneOffset(zone: string | null, at: Date = new Date()): number {
  if (!zone) return -at.getTimezoneOffset();
  const parts = new Intl.DateTimeFormat("en-US", {
    timeZone: zone,
    hourCycle: "h23",
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).formatToParts(at);
  const get = (t: string) => Number(parts.find((p) => p.type === t)?.value ?? 0);
  const local = Date.UTC(get("year"), get("month") - 1, get("day"), get("hour"), get("minute"));
  const utc = Date.UTC(at.getUTCFullYear(), at.getUTCMonth(), at.getUTCDate(), at.getUTCHours(), at.getUTCMinutes());
  return Math.round((local - utc) / 60000);
}

/** "HH:MM" 加上 minutes 分钟，跨午夜时绕回；格式不对原样返回 */
export function shiftTime(hhmm: string, minutes: number): string {
  const m = /^(\d{1,2}):(\d{2})$/.exec(hhmm.trim());
  if (!m) return hhmm;
  const total = (((Number(m[1]) * 60 + Number(m[2]) + minutes) % 1440) + 1440) % 1440;
  return `${String(Math.floor(total / 60)).padStart(2, "0")}:${String(total % 60).padStart(2, "0")}`;
}

/** 从 from 时区换到 to 时区时，时刻要加多少分钟（北京 → 东京是 +60） */
export function zoneShift(from: string | null, to: string | null, at: Date = new Date()): number {
  return zoneOffset(to, at) - zoneOffset(from, at);
}
