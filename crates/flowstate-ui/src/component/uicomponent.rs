trait UiComponent {
    fn layout(&mut self, ctx: &LayoutCtx);
    fn elements(&self) -> &[UiElement];
    fn render(&self, renderer: &mut RenderCtx);
}y
