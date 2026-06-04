import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  Button,
  FluentProvider,
  Input,
  Tooltip,
  webDarkTheme,
  webLightTheme,
} from "@fluentui/react-components";
import {
  ClipboardRegular,
  CodeRegular,
  CopyRegular,
  DeleteRegular,
  DismissRegular,
  ImageRegular,
  LinkRegular,
  PinFilled,
  PinRegular,
  SearchRegular,
  WeatherMoonRegular,
  WeatherSunnyRegular,
} from "@fluentui/react-icons";
import "./App.css";

type ClipType = "text" | "link" | "code" | "image";
type Filter = "all" | "pinned" | ClipType;

type Clip = {
  id: number;
  clipType: ClipType;
  content: string;
  source: string;
  createdAt: number;
  pinned: boolean;
  image?: string;
};

const filters: { value: Filter; label: string }[] = [
  { value: "all", label: "Tất cả" },
  { value: "pinned", label: "Đã ghim" },
  { value: "text", label: "Văn bản" },
  { value: "image", label: "Hình ảnh" },
];

const typeLabel: Record<ClipType, string> = {
  text: "Văn bản",
  link: "Liên kết",
  code: "Mã nguồn",
  image: "Hình ảnh",
};

function TypeIcon({ type }: { type: ClipType }) {
  if (type === "link") return <LinkRegular />;
  if (type === "code") return <CodeRegular />;
  if (type === "image") return <ImageRegular />;
  return <ClipboardRegular />;
}

function relativeTime(timestamp: number) {
  const minutes = Math.floor(Math.max(0, Date.now() - timestamp) / 60_000);
  if (minutes < 1) return "Vừa xong";
  if (minutes < 60) return `${minutes} phút trước`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} giờ trước`;
  return new Intl.DateTimeFormat("vi-VN", {
    day: "2-digit",
    month: "2-digit",
  }).format(timestamp);
}

function App() {
  const [darkMode, setDarkMode] = useState(false);
  const [clips, setClips] = useState<Clip[]>([]);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [copiedId, setCopiedId] = useState<number | null>(null);

  useEffect(() => {
    invoke<Clip[]>("get_history").then(setClips).catch(console.error);
    const unlisten = listen<Clip[]>("clipboard-history-updated", (event) => {
      setClips(event.payload);
    });
    return () => void unlisten.then((dispose) => dispose());
  }, []);

  const filteredClips = useMemo(
    () =>
      clips.filter((clip) => {
        const matchesFilter =
          filter === "all" ||
          (filter === "pinned" ? clip.pinned : clip.clipType === filter);
        return (
          matchesFilter &&
          clip.content.toLocaleLowerCase().includes(query.toLocaleLowerCase())
        );
      }),
    [clips, filter, query],
  );

  const copyClip = async (clip: Clip) => {
    await invoke("copy_clip", { id: clip.id });
    setCopiedId(clip.id);
    window.setTimeout(() => setCopiedId(null), 1200);
  };

  const togglePin = async (id: number) => {
    setClips(await invoke<Clip[]>("toggle_pin", { id }));
  };

  const deleteClip = async (id: number) => {
    setClips(await invoke<Clip[]>("delete_clip", { id }));
  };

  return (
    <FluentProvider theme={darkMode ? webDarkTheme : webLightTheme}>
      <main className="app">
        <header className="header">
          <div>
            <h1>Clipboard</h1>
            <span>{clips.length} mục đã lưu</span>
          </div>
          <div className="header-actions">
            <Tooltip content="Xóa mục chưa ghim" relationship="label">
              <Button
                appearance="subtle"
                size="small"
                icon={<DeleteRegular />}
                onClick={async () =>
                  setClips(await invoke<Clip[]>("clear_unpinned"))
                }
              />
            </Tooltip>
            <Tooltip content="Đổi giao diện" relationship="label">
              <Button
                appearance="subtle"
                size="small"
                icon={darkMode ? <WeatherSunnyRegular /> : <WeatherMoonRegular />}
                onClick={() => setDarkMode((value) => !value)}
              />
            </Tooltip>
          </div>
        </header>

        <section className="controls">
          <Input
            className="search"
            size="medium"
            contentBefore={<SearchRegular />}
            contentAfter={
              query ? (
                <Button
                  appearance="transparent"
                  size="small"
                  icon={<DismissRegular />}
                  aria-label="Xóa tìm kiếm"
                  onClick={() => setQuery("")}
                />
              ) : null
            }
            placeholder="Tìm nội dung đã sao chép"
            value={query}
            onChange={(_, data) => setQuery(data.value)}
          />
          <div className="filters">
            {filters.map((item) => (
              <button
                className={filter === item.value ? "active" : ""}
                key={item.value}
                onClick={() => setFilter(item.value)}
              >
                {item.label}
              </button>
            ))}
          </div>
        </section>

        <section className="history">
          {filteredClips.map((clip) => (
            <article
              className="clip-row"
              key={clip.id}
              onDoubleClick={() => copyClip(clip)}
            >
              {clip.image ? (
                <img className="preview" src={clip.image} alt={clip.content} />
              ) : (
                <div className={`type-icon ${clip.clipType}`}>
                  <TypeIcon type={clip.clipType} />
                </div>
              )}

              <div className="clip-content">
                <p>{clip.content || "Hình ảnh từ clipboard"}</p>
                <span>
                  {typeLabel[clip.clipType]} · {relativeTime(clip.createdAt)}
                </span>
              </div>

              <div className="row-actions">
                <Button
                  appearance={copiedId === clip.id ? "primary" : "subtle"}
                  size="small"
                  icon={<CopyRegular />}
                  aria-label="Sao chép lại"
                  onClick={() => copyClip(clip)}
                />
                <Button
                  appearance="subtle"
                  size="small"
                  icon={clip.pinned ? <PinFilled /> : <PinRegular />}
                  aria-label={clip.pinned ? "Bỏ ghim" : "Ghim"}
                  onClick={() => togglePin(clip.id)}
                />
                <Button
                  appearance="subtle"
                  size="small"
                  icon={<DeleteRegular />}
                  aria-label="Xóa"
                  onClick={() => deleteClip(clip.id)}
                />
              </div>
            </article>
          ))}

          {filteredClips.length === 0 && (
            <div className="empty">
              <ClipboardRegular />
              <strong>Chưa có nội dung</strong>
              <span>Hãy sao chép văn bản hoặc hình ảnh.</span>
            </div>
          )}
        </section>
      </main>
    </FluentProvider>
  );
}

export default App;
