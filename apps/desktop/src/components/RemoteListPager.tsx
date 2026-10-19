export function RemoteListPager({ page, pageSize, count, onPage }: {
  page: number; pageSize: number; count: number; onPage: (page: number) => void;
}) {
  const pages = Math.max(1, Math.ceil(count / pageSize));
  if (pages === 1) return null;
  return <nav className="remote-list-pager" aria-label="列表分页">
    <button type="button" className="button-secondary" disabled={page === 0} onClick={() => onPage(page - 1)}>上一页</button>
    <span>{page + 1} / {pages} 页 · 每页 {pageSize} 条</span>
    <button type="button" className="button-secondary" disabled={page + 1 >= pages} onClick={() => onPage(page + 1)}>下一页</button>
  </nav>;
}
