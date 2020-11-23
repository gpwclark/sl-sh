(load "parse-docstrings.lisp")
(ns-push 'docmd)

(ns-import 'iterator)
(ns-import 'shell)
(ns-import 'docparse)

(defn create-header (index-file)
	(var new-file (open index-file :create :truncate))
	(write-string new-file "---
layout: default
title: Sl-sh form documentation
---

# Sl-sh

")
	(close new-file))

(defn section-metadata (key attr)
	;; TODO why does this need to be stringified?
	(var idx (match attr
			(:name 0)
			(:description 1)
			(nil (err "Unknown attribute of table heading."))))
	(match (str key)
		("sequence" (vec-nth '#("Sequence forms"
"These macros will work on either a vector or a pair made into a proper list
(cons list).  Use these in preference to the vector/list specific versions when
possible (i.e. first vs car).
NOTE: list on this table can be a vector or a list.") idx))
		("char" (vec-nth '#("Char forms" nil) idx))
		("conditional" (vec-nth '#("Conditional forms" nil) idx))
		("core" (vec-nth '#("Core forms" nil) idx))
		("file" (vec-nth '#("File forms"
" Options to open, one or more of these can be added to open after the filename.
A file can only be opened for reading or writing (read is default).

Option | Description
-------|-----------
:read | Open file for reading, this is the default.
:write | Open file for writing.
:append | Open file for writing and append new data to end.
:truncate | Open file for write and delete all existing data.
:create | Create the file if it does not exist and open for writing.
:create-new | Create if does not exist, error if it does and open for writing.
:on-error-nil | If open has an error then return nil instead of producing an error.

Notes on closing.  Files will close when they go out of scope.  Using close will
cause a reference to a file to be marked close (removes that reference).  If
there are more then one references to a file it will not actually close until
all are released.  Close will also flush the file even if it is not the final
reference.  If a reference to a file is captured in a closure that can also keep
it open (closures currently capture the entire scope not just used symbols).") idx))
		("hashmap" (vec-nth '#("Hashmap forms" nil) idx))
		("scripting" (vec-nth '#("Scripting forms" nil) idx))
		("math" (vec-nth '#("Math forms" nil) idx))
		("namespace" (vec-nth '#("Namespace forms" nil) idx))
		("pair" (vec-nth '#("Pair forms"
"Operations on the 'Pair' type (aka Cons Cell) that can be used to create
traditional Lisp list structures. These are the default list structure and
are produced with bare parentheses in code. These lists can also be created by
building them up with joins or with the list form.") idx))
		("shell" (vec-nth '#("Shell forms"
"Forms to do shell operations like file tests, pipes, redirects, etc.") idx))
		("string" (vec-nth '#("String forms" nil) idx))
		("type" (vec-nth '#("Type forms"
"These forms provide information/tests about an objects underlying type.") idx))
		("vector" (vec-nth '#("Vector forms"
"Forms ending in '!' are destructive and change the underlying vector, other forms
do not make changes to the the provided vector.  They are usable in place of a
list for purposes of lambda calls, parameters, etc (they work the same as a list
made from pairs but are vectors not linked lists).  Use #() to declare them in
code (i.e. '#(1 2 3) or #(+ 1 2)).") idx))
		(":uncategorized" (vec-nth '#("Uncategorized forms" nil) idx))
		(nil (if (= idx 0) (str key " forms") ""))))

(defn create-anchor (id)
	(str "<a id=\"" id "\" class=\"anchor\" aria-hidden=\"true\" href=\"#sl-sh-form-documentation\"></a>"))

(defn make-md-link-able (link-display-text link)
	(str "[" link-display-text "](" link ")"))

(defn write-heading (heading file-name)
	(var file (open file-name :append))
	(write-line file "")
	(write-line file (str "## " heading))
	(write-line file "")
	(close file)
	file-name)

(defn write-version (file-name)
	(var file (open file-name :append))
	(write-line file "")
	(write-line file (str "version: " (version)))
	(write-line file "")
	(close file)
	file-name)

(defn get-anchor-link-id (doc-map)
	(var doc-form (do
		(var form (hash-get doc-map :form))
		(if (= "\|" form) "pipe-shorthand" form)))
	(var doc-namespace (hash-get doc-map :namespace))
	(str doc-namespace "::" doc-form))

(defn table-of-contents (key docstrings file-name)
	(var file (open file-name :append))
	(var name (section-metadata key :name))
	(write-line file (str "### "
		(create-anchor (str name "-contents" ))
		(make-md-link-able name (str "#" name "-body"))))
	(write-line file "")
	(write-line file "")
	(var is-first #t)
	(for doc-map in docstrings (do
		(if is-first
			(set! is-first nil)
			(write-string file ", "))
		(var doc-form (do
			(var form (hash-get doc-map :form))
			(if (= "\|" form) "|" form)))
		(var doc-namespace (hash-get doc-map :namespace))
		(write-string file
			(str
				(create-anchor (str (get-anchor-link-id doc-map) "-contents"))
				(make-md-link-able (str "``" doc-form "``") (str "#" (get-anchor-link-id doc-map)))))))
	(write-line file "")
	(close file)
	file-name)

(defn doc-structure (file-name)
	(var file (open file-name :append))
	(write-line file "")
	(write-line file "")
	(write-line file
		(str "| <b>form name</b> | <b>type</b> (see: "
			 (make-md-link-able (section-metadata "type" :name) (str "#" (section-metadata "type" :name) "-contents"))
			 ") |"))
	(write-line file "| <b>namespace</b> (fully qualified names are of format namespace::symbol) | <b>usage</b> |")
	(write-line file "")
	(write-line file "```")
	(write-line file "example code if exists")
	(write-line file "```")
	(close file)
	file-name)

(defn check-if-pipe-shorthand (item)
	(if (= item "(\| &rest body)") "(| &rest body)" item))

(defn format-first-line-as-code (text-slice delim)
	(if (or (nil? text-slice) (str-empty? (str-trim text-slice)))
	text-slice
	(do
	(var arr (str-split delim text-slice))
	;; if first char in str is delim, 0th elem is "" when we don't need to
	;; bracket with backticks
	(var trim-arr (if (= "" (first arr)) (rest arr) arr))
	(str-cat-list delim (collect-vec (append (list (str "``" (check-if-pipe-shorthand (first trim-arr)) "``")) (rest trim-arr)))))))

(defn sanitize-for-md-row (to-sanitize)
		(str-replace to-sanitize "|" "\|"))

(defn write-md-table (key docstrings file-name)
	(var file (open file-name :append))
	(var name (section-metadata key :name))
	(write-line file (str "### "
				(create-anchor (str name "-body"))
				(make-md-link-able name (str "#" name "-contents"))))
	(write-line file (do
		 (var data (section-metadata key :description))
		 (if (nil? data)
			""
			data)))
	(for doc-map in docstrings (do
		(var doc-form (do
			(var form (hash-get doc-map :form))
			(if (= "\|" form) "|" (sanitize-for-md-row form))))
		(var doc-namespace (sanitize-for-md-row (hash-get doc-map :namespace)))
		(var doc-type (sanitize-for-md-row (hash-get doc-map :type)))
		(var doc-usage (str-replace (sanitize-for-md-row (hash-get doc-map :usage)) "\n" "<br>"))
		(var doc-example (hash-get doc-map :example))
		(write-line file "")
		(write-line file "")
		(write-line file
			(str "| "
					(create-anchor (str (get-anchor-link-id doc-map)))
					(make-md-link-able (str "``" doc-form "``")
					(str "#" (get-anchor-link-id doc-map) "-contents"))
				" | " doc-type " |"))
		(write-line file 
			(str "| ``" doc-namespace "::" doc-form
				 "`` | " (format-first-line-as-code doc-usage "<br>") " |"))
		(write-line file "")
		(if (not (nil? doc-example))
			(do
                (write-line file "<details style=\"padding-bottom: 5px;\">")
                (write-line file "<summary>⮞</summary>")
				(write-line file "<code>")
					(for line in (str-split "\n" doc-example) (write-line file line))
				(write-line file "</code>")
                (write-line file "</details>"))
            (write-line file "<br>"))))
	(close file)
	file-name)

(defn make-md-file
	  ;; TODO should be using get-error instead of do
	(index-file sym-list) (do
	(var docstrings-map (parse-docstrings-for-syms sym-list))
	(create-header index-file)
	;; explain format
	(write-heading "Documentation structure for each form" index-file)
	(doc-structure index-file)
	;; generate table of contents
	(write-heading "Table of Contents" index-file)
	(for key in (qsort (hash-keys docstrings-map)) (do
		(var docstrings (hash-get docstrings-map key))
		(table-of-contents key docstrings index-file)))
	;; generate markdown body
	(write-heading "Documentation" index-file)
	(for key in (qsort (hash-keys docstrings-map)) (do
		(var docstrings (hash-get docstrings-map key))
		(write-md-table key docstrings index-file)))
	(write-version index-file)
	(do
		(var uncat-syms (hash-get docstrings-map :uncategorized))
		(when (not (empty-seq? uncat-syms)) (do
			(println "Found :uncategorized symbols: ")
			(for symbol in uncat-syms (println "symbol: " symbol))
			nil))
		 #t)))

(ns-export '(make-md-file))
(ns-pop)
