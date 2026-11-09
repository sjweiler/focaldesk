(()=>{
  const report=()=>parent.postMessage({type:'mail-size',height:Math.min(20000,Math.max(220,document.documentElement.scrollHeight))},'*');
  for(const event of ['click','auxclick'])document.addEventListener(event,e=>{
    const link=e.target instanceof Element?e.target.closest('a[href]'):null;
    if(link){e.preventDefault();parent.postMessage({type:'mail-link',url:link.getAttribute('href')},'*');}
  });
  new ResizeObserver(report).observe(document.body);
  window.addEventListener('load',report);report();
})();
