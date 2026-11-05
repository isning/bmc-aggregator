use syn::visit::{self, Visit};

#[derive(Default)]
struct PureModelCheck {
    violations: Vec<String>,
}
impl<'ast> Visit<'ast> for PureModelCheck {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        if let Some(first) = path.segments.first() {
            let root = first.ident.to_string();
            if ["std", "tokio", "tracing", "gstreamer", "reqwest", "rand"].contains(&root.as_str())
            {
                self.violations.push(format!("runtime path: {root}"));
            }
        }
        if path.segments.iter().any(|segment| {
            ["atomic", "cell", "sync", "arch", "Fn", "FnMut", "FnOnce"]
                .contains(&segment.ident.to_string().as_str())
        }) {
            self.violations
                .push("shared mutable state or injected behavior".into());
        }
        visit::visit_path(self, path);
    }
    fn visit_item_extern_crate(&mut self, item: &'ast syn::ItemExternCrate) {
        if item.ident != "alloc" {
            self.violations.push("external runtime crate".into());
        }
    }
    fn visit_type_bare_fn(&mut self, _: &'ast syn::TypeBareFn) {
        self.violations.push("injected callback".into());
    }
    fn visit_type_trait_object(&mut self, _: &'ast syn::TypeTraitObject) {
        self.violations.push("dynamic behavior".into());
    }
    fn visit_type_impl_trait(&mut self, _: &'ast syn::TypeImplTrait) {
        self.violations.push("opaque injected behavior".into());
    }
    fn visit_item_foreign_mod(&mut self, _: &'ast syn::ItemForeignMod) {
        self.violations.push("foreign effects".into());
    }
    fn visit_item_macro(&mut self, item: &'ast syn::ItemMacro) {
        if item.mac.path.is_ident("macro_rules")
            && item
                .ident
                .as_ref()
                .is_none_or(|name| name != "finite_states")
        {
            self.violations.push("unreviewed macro definition".into());
        }
        visit::visit_item_macro(self, item);
    }
    fn visit_item_static(&mut self, _: &'ast syn::ItemStatic) {
        self.violations.push("global state".into());
    }
    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        fn root(tree: &syn::UseTree) -> Option<String> {
            match tree {
                syn::UseTree::Path(path) => Some(path.ident.to_string()),
                syn::UseTree::Name(name) => Some(name.ident.to_string()),
                syn::UseTree::Rename(rename) => Some(rename.ident.to_string()),
                syn::UseTree::Glob(_) | syn::UseTree::Group(_) => None,
            }
        }
        if !root(&item.tree).is_some_and(|name| {
            ["alloc", "core", "crate", "self", "super"].contains(&name.as_str())
        }) {
            self.violations.push("import outside the pure model".into());
        }
        visit::visit_item_use(self, item);
    }
    fn visit_signature(&mut self, signature: &'ast syn::Signature) {
        if signature.asyncness.is_some() {
            self.violations.push("async decision function".into());
        }
        visit::visit_signature(self, signature);
    }
    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if matches!(item.vis, syn::Visibility::Public(_)) {
            for input in &item.sig.inputs {
                if matches!(input, syn::FnArg::Typed(argument) if matches!(&*argument.ty, syn::Type::Reference(reference) if reference.mutability.is_some()))
                {
                    self.violations.push("public mutable input".into());
                }
            }
            if !item.sig.generics.params.is_empty() {
                self.violations
                    .push("unreviewed generic public behavior".into());
            }
        }
        visit::visit_item_fn(self, item);
    }
    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if matches!(item.vis, syn::Visibility::Public(_)) {
            if !item.sig.generics.params.is_empty() {
                self.violations
                    .push("unreviewed generic public behavior".into());
            }
            for arg in &item.sig.inputs {
                match arg {
                    syn::FnArg::Receiver(receiver)
                        if receiver.reference.is_some() && receiver.mutability.is_some() =>
                    {
                        self.violations.push("public mutable receiver".into())
                    }
                    syn::FnArg::Typed(arg) if matches!(&*arg.ty, syn::Type::Reference(reference) if reference.mutability.is_some()) => {
                        self.violations.push("public mutable input".into())
                    }
                    _ => {}
                }
            }
        }
        visit::visit_impl_item_fn(self, item);
    }
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if [
            "load",
            "store",
            "fetch_add",
            "fetch_sub",
            "lock",
            "read",
            "write",
            "elapsed",
        ]
        .contains(&call.method.to_string().as_str())
        {
            self.violations
                .push("ambient state, time, or synchronization".into());
        }
        visit::visit_expr_method_call(self, call);
    }
    fn visit_macro(&mut self, item: &'ast syn::Macro) {
        let name = item.path.segments.last().unwrap().ident.to_string();
        if ![
            "macro_rules",
            "finite_states",
            "format",
            "matches",
            "assert",
            "assert_eq",
            "assert_ne",
            "panic",
            "vec",
        ]
        .contains(&name.as_str())
        {
            self.violations.push(format!("unreviewed macro: {name}"));
        }
    }
    fn visit_attribute(&mut self, item: &'ast syn::Attribute) {
        if item.path().is_ident("path") {
            self.violations.push("hidden source module".into());
        }
        visit::visit_attribute(self, item);
    }
}
pub fn violations(source: &str) -> Vec<String> {
    let syntax = syn::parse_file(source).expect("valid Rust source");
    let mut check = PureModelCheck::default();
    check.visit_file(&syntax);
    check.violations
}
