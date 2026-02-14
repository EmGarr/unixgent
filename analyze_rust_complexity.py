#!/usr/bin/env python3
import re
import os
import sys

def count_lines_and_nesting(content, start_line, end_line):
    """Count lines and maximum nesting depth for a function"""
    lines = content.split('\n')[start_line:end_line+1]
    max_depth = 0
    current_depth = 0
    
    for line in lines:
        # Count opening braces, if/for/while/match blocks
        opening = line.count('{')
        closing = line.count('}')
        
        # Also count control structures that create nesting
        if re.search(r'\b(if|for|while|loop|match)\b', line) and not line.strip().startswith('//'):
            if '{' not in line:  # If no opening brace on same line, it's still nesting
                current_depth += 1
        
        current_depth += opening
        current_depth -= closing
        max_depth = max(max_depth, current_depth)
    
    return len(lines), max_depth

def estimate_cyclomatic_complexity(content, start_line, end_line):
    """Estimate cyclomatic complexity by counting decision points"""
    lines = content.split('\n')[start_line:end_line+1]
    complexity = 1  # Base complexity
    
    for line in lines:
        line = line.strip()
        if line.startswith('//'):
            continue
            
        # Count decision points
        complexity += line.count('if ')
        complexity += line.count(' if ')
        complexity += line.count('else if')
        complexity += line.count('while ')
        complexity += line.count('for ')
        complexity += line.count('loop ')
        complexity += line.count('match ')
        complexity += line.count('&&')
        complexity += line.count('||')
        complexity += line.count('?')
        complexity += line.count('.unwrap_or')
        complexity += line.count('.map_or')
        
        # Count match arms
        if '=>' in line and not line.strip().startswith('//'):
            complexity += 1
    
    return complexity

def analyze_file(filepath):
    """Analyze a Rust file for function complexity"""
    try:
        with open(filepath, 'r', encoding='utf-8') as f:
            content = f.read()
    except Exception as e:
        print(f"Error reading {filepath}: {e}")
        return []
    
    issues = []
    lines = content.split('\n')
    
    # Find function definitions
    fn_pattern = r'^(\s*)(?:pub\s+)?(?:async\s+)?fn\s+(\w+)'
    
    i = 0
    while i < len(lines):
        line = lines[i]
        match = re.match(fn_pattern, line)
        
        if match:
            indent_level = len(match.group(1))
            fn_name = match.group(2)
            start_line = i
            
            # Find the end of the function by tracking braces
            brace_count = 0
            found_opening = False
            end_line = start_line
            
            # Look for opening brace
            for j in range(i, len(lines)):
                if '{' in lines[j]:
                    found_opening = True
                    brace_count += lines[j].count('{')
                    brace_count -= lines[j].count('}')
                    end_line = j
                    break
                elif ';' in lines[j]:
                    # Function declaration without body
                    break
            
            if found_opening:
                # Continue until braces are balanced
                for j in range(end_line + 1, len(lines)):
                    brace_count += lines[j].count('{')
                    brace_count -= lines[j].count('}')
                    end_line = j
                    if brace_count == 0:
                        break
                
                # Analyze the function
                line_count, max_nesting = count_lines_and_nesting(content, start_line, end_line)
                complexity = estimate_cyclomatic_complexity(content, start_line, end_line)
                
                # Check for issues
                function_issues = []
                
                if line_count > 50:
                    function_issues.append(f"Long function: {line_count} lines")
                
                if max_nesting > 4:
                    function_issues.append(f"Deep nesting: {max_nesting} levels")
                
                if complexity > 10:
                    function_issues.append(f"High cyclomatic complexity: {complexity}")
                
                if function_issues:
                    issues.append({
                        'function': fn_name,
                        'start_line': start_line + 1,
                        'end_line': end_line + 1,
                        'line_count': line_count,
                        'max_nesting': max_nesting,
                        'complexity': complexity,
                        'issues': function_issues
                    })
            
            i = end_line + 1
        else:
            i += 1
    
    return issues

def main():
    rust_files = []
    for root, dirs, files in os.walk('./crates'):
        for file in files:
            if file.endswith('.rs'):
                rust_files.append(os.path.join(root, file))
    
    all_issues = []
    
    for filepath in rust_files:
        issues = analyze_file(filepath)
        if issues:
            all_issues.append((filepath, issues))
    
    # Report results
    if not all_issues:
        print("No complexity issues found in any Rust files.")
        return
    
    print("RUST CODE COMPLEXITY ANALYSIS REPORT")
    print("=" * 50)
    
    total_issues = 0
    for filepath, issues in all_issues:
        print(f"\nFile: {filepath}")
        print("-" * len(f"File: {filepath}"))
        
        for issue in issues:
            total_issues += 1
            print(f"  Function: {issue['function']} (lines {issue['start_line']}-{issue['end_line']})")
            print(f"    Lines: {issue['line_count']}, Max Nesting: {issue['max_nesting']}, Complexity: {issue['complexity']}")
            for problem in issue['issues']:
                print(f"    ⚠️  {problem}")
            print()
    
    print(f"Summary: Found {total_issues} functions with complexity issues across {len(all_issues)} files.")

if __name__ == "__main__":
    main()
